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
    CoordinatorAdmission, CoordinatorCommandResult, ExecutionProfile, TOOL_APPROVAL_TTL_MILLIS,
    TurnAdmission, TurnCommand,
    task_completion_notifier::TaskCompletionNotifier,
    task_execution_dispatcher::{
        ExecutionPipeline, LlmTaskDispatcher, LlmTaskDispatcherDependencies,
    },
    task_execution_registry::AgentSpawnPreflightRuntime,
    task_runner_bridge::EventBasedResultReceiver,
};
use magi_core::{AccessProfile, SessionId, UtcMillis};
use magi_event_bus::{EventEnvelope, InMemoryEventBus};
use magi_governance::GovernanceService;
use magi_memory_store::MemoryStore;
use magi_orchestrator::{OrchestratorService, task_store::TaskStore};
use magi_session_store::{
    ActiveExecutionTurn, CanonicalTurn, CanonicalTurnItemKind, SessionStore, TimelineEntryInput,
    TimelineEntryKind,
};
use magi_skill_runtime::SkillDispatchRuntime;
use magi_tool_runtime::ToolRegistry;
use magi_worker_runtime::WorkerRuntime;
use magi_workspace::WorkspaceStore;
use std::time::{Duration, Instant};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, atomic::AtomicU64},
};
use tower::ServiceExt;

#[derive(Clone, Default)]
struct HarnessTiming {
    state: Arc<Mutex<HarnessTimingState>>,
}

#[derive(Default)]
struct HarnessTimingState {
    submit_started_at: Option<Instant>,
    accepted_returned_at: Option<Instant>,
    provider_request_started_at: Option<Instant>,
    provider_first_delta_at: Option<Instant>,
    first_stream_event_at: Option<Instant>,
    first_stream_event_sequence: Option<u64>,
    terminal_observed_at: Option<Instant>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HarnessTimingSnapshot {
    pub accepted_returned_ms: Option<u128>,
    pub provider_request_started_ms: Option<u128>,
    pub provider_first_delta_ms: Option<u128>,
    pub first_stream_event_ms: Option<u128>,
    pub first_stream_event_sequence: Option<u64>,
    pub terminal_observed_ms: Option<u128>,
}

impl HarnessTiming {
    fn reset(&self) {
        *self.state.lock().expect("harness timing state should hold") =
            HarnessTimingState::default();
    }

    fn mark_submit_started(&self) {
        let mut state = self.state.lock().expect("harness timing state should hold");
        if state.submit_started_at.is_none() {
            state.submit_started_at = Some(Instant::now());
        }
    }

    fn mark_accepted_returned(&self) {
        let mut state = self.state.lock().expect("harness timing state should hold");
        if state.accepted_returned_at.is_none() {
            state.accepted_returned_at = Some(Instant::now());
        }
    }

    fn mark_provider_request_started(&self) {
        let mut state = self.state.lock().expect("harness timing state should hold");
        if state.provider_request_started_at.is_none() {
            state.provider_request_started_at = Some(Instant::now());
        }
    }

    fn mark_provider_first_delta(&self) {
        let mut state = self.state.lock().expect("harness timing state should hold");
        if state.provider_first_delta_at.is_none() {
            state.provider_first_delta_at = Some(Instant::now());
        }
    }

    fn mark_first_stream_event(&self, sequence: u64) {
        let mut state = self.state.lock().expect("harness timing state should hold");
        if state.first_stream_event_at.is_none() {
            state.first_stream_event_at = Some(Instant::now());
            state.first_stream_event_sequence = Some(sequence);
        }
    }

    fn mark_terminal_observed(&self) {
        let mut state = self.state.lock().expect("harness timing state should hold");
        if state.terminal_observed_at.is_none() {
            state.terminal_observed_at = Some(Instant::now());
        }
    }

    fn snapshot(&self) -> HarnessTimingSnapshot {
        let state = self.state.lock().expect("harness timing state should hold");
        let Some(started_at) = state.submit_started_at else {
            return HarnessTimingSnapshot::default();
        };
        let elapsed = |at: Option<Instant>| at.map(|at| at.duration_since(started_at).as_millis());
        HarnessTimingSnapshot {
            accepted_returned_ms: elapsed(state.accepted_returned_at),
            provider_request_started_ms: elapsed(state.provider_request_started_at),
            provider_first_delta_ms: elapsed(state.provider_first_delta_at),
            first_stream_event_ms: elapsed(state.first_stream_event_at),
            first_stream_event_sequence: state.first_stream_event_sequence,
            terminal_observed_ms: elapsed(state.terminal_observed_at),
        }
    }
}

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
    tool_round_emitted: usize,
    tool_round_limit: usize,
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
    timing: HarnessTiming,
}

impl HarnessModelClient {
    pub fn new(response: impl Into<String>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ProviderState {
                behavior: Some(ProviderBehavior::Completed(response.into())),
                ..ProviderState::default()
            })),
            timing: HarnessTiming::default(),
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
        state.tool_round_emitted = 0;
        state.tool_round_limit = 1;
    }

    pub fn set_tool_then_completed_twice(
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
        state.tool_round_emitted = 0;
        state.tool_round_limit = 2;
    }

    pub fn requests(&self) -> Vec<ModelInvocationRequest> {
        self.state
            .lock()
            .expect("harness provider state should hold")
            .requests
            .clone()
    }

    /// 返回最近一次通过该 Provider 执行的 Turn 时间线。
    ///
    /// 这是单轮观测证据，不能替代多轮 P50/P95 采样。
    pub fn timing(&self) -> HarnessTimingSnapshot {
        self.timing.snapshot()
    }

    fn record_delta(
        &self,
        delta: &ModelStreamingDelta,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        track_timing: bool,
    ) {
        if track_timing && (!delta.content.is_empty() || !delta.thinking.is_empty()) {
            self.timing.mark_provider_first_delta();
        }
        self.state
            .lock()
            .expect("harness provider state should hold")
            .deltas
            .push(delta.clone());
        on_delta(delta);
    }

    fn begin_timing(&self) {
        self.timing.reset();
        self.timing.mark_submit_started();
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
        let track_timing = !request.prompt.contains("Session Turn 编排分类器");
        if track_timing {
            self.timing.mark_provider_request_started();
        }
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
                    self.record_delta(&delta, on_delta, track_timing);
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
                    self.record_delta(&delta, on_delta, track_timing);
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
                    self.record_delta(&delta, on_delta, track_timing);
                }
                Ok(ModelResponse::completed(content))
            }
            ProviderBehavior::ToolThenCompleted {
                tool_name,
                arguments,
                response,
            } => {
                let tool_round_index = {
                    let mut state = self
                        .state
                        .lock()
                        .expect("harness provider state should hold");
                    let classifier_request = request.prompt.contains("Session Turn 编排分类器");
                    let tool_surface_available = request.tools.as_ref().is_some_and(|tools| {
                        tools.iter().any(|tool| tool.function.name == tool_name)
                    });
                    if !classifier_request
                        && tool_surface_available
                        && state.tool_round_emitted < state.tool_round_limit
                    {
                        state.tool_round_emitted += 1;
                        Some(state.tool_round_emitted)
                    } else {
                        None
                    }
                };
                if let Some(tool_round_index) = tool_round_index {
                    return Ok(ModelResponse {
                        status: ModelResponseStatus::RequiresToolExecution,
                        content: None,
                        thinking: None,
                        tool_calls: vec![ChatToolCall {
                            id: format!("harness-tool-call-{tool_round_index}"),
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
                    self.record_delta(&delta, on_delta, track_timing);
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
            self.timing.mark_provider_request_started();
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
        // 普通 Conversation harness 不创建 TaskStore；这与生产 profile 边界一致，
        // 不能只是不把已经创建的 TaskStore 挂到 ApiState 上。
        let task_store = with_task_runtime.then(|| Arc::new(TaskStore::new()));
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
        if let Some(task_store) = task_store.as_ref() {
            execution_runtime = execution_runtime.with_task_store(Arc::clone(&task_store));
        }
        let result_receiver = Arc::new(EventBasedResultReceiver::new());
        let completion_notifier = task_store
            .as_ref()
            .map(|task_store| Arc::new(TaskCompletionNotifier::new(Arc::clone(task_store))));
        if let Some(notifier) = completion_notifier.as_ref() {
            let notifier_for_status = notifier.clone();
            task_store
                .as_ref()
                .expect("TaskStore 应与主动完成通知器同时存在")
                .set_status_change_callback(Box::new(
                    move |task_id, _old_status, new_status, task| {
                        notifier_for_status.notify_terminal_status(task_id, new_status, &task);
                    },
                ));
        }
        let mut state = ApiState::new(
            "magi-turn-harness",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            workspace_store,
            governance,
        )
        .with_tool_registry(tool_registry.clone());
        if let Some(task_store) = task_store.as_ref() {
            state = state.with_task_store(Arc::clone(task_store));
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
            let task_store = task_store
                .as_ref()
                .expect("TaskStore 应与 RunnerManager 同时存在");
            let runner_manager = crate::state::RunnerManager::with_dispatcher_and_worker_catalog(
                Arc::clone(task_store),
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

    /// 返回本轮前后可观察的 Task/Runner/Snapshot/Git 运行时资源状态。
    /// Conversation profile 必须保持四项都为零或未物化。
    pub fn conversation_runtime_resource_state(
        &self,
        session_id: &SessionId,
        workspace_root: Option<&Path>,
    ) -> (usize, usize, bool, bool) {
        let task_count = self
            .state
            .task_store()
            .map(|store| store.all_tasks().len())
            .unwrap_or(0);
        let runner_count = if self.state.runner_manager().is_some() {
            1
        } else {
            0
        };
        let snapshot_created = workspace_root
            .and_then(|root| self.state.snapshot_session(session_id, root))
            .is_some();
        let git_context_created = self
            .state
            .session_code_contexts
            .get(session_id.as_str())
            .is_some();
        (
            task_count,
            runner_count,
            snapshot_created,
            git_context_created,
        )
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
        self.provider.begin_timing();
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
        self.provider.begin_timing();
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
        let response = TurnService::new(self.state.clone())
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
            .await;
        if response.is_ok() {
            self.provider.timing.mark_accepted_returned();
        }
        response
    }

    pub async fn submit_task(
        &self,
        text: &str,
        request_id: &str,
        user_message_id: &str,
    ) -> Result<SessionTurnResponseDto, crate::errors::ApiError> {
        self.submit(None, text, request_id, user_message_id).await
    }

    /// 在已注册 workspace 中通过真实 TurnService 提交 Task profile 请求。
    ///
    /// 该入口只存在于测试 harness，用于把 workspace/Git 前置检查纳入真实
    /// Turn 接纳链；生产代码仍由 HTTP/App Server 的同一份 DTO 合同接收请求。
    pub async fn submit_workspace_task(
        &self,
        session_id: &SessionId,
        workspace_id: &magi_core::WorkspaceId,
        workspace_path: &Path,
        text: &str,
        request_id: &str,
        user_message_id: &str,
    ) -> Result<SessionTurnResponseDto, crate::errors::ApiError> {
        self.submit_workspace_task_with_access_profile(
            session_id,
            workspace_id,
            workspace_path,
            text,
            request_id,
            user_message_id,
            None,
        )
        .await
    }

    /// 在已注册 workspace 中通过真实 TurnService 提交普通 Conversation profile 请求。
    /// 该入口用于验证工作区聊天不会因为携带 workspace 作用域而隐式创建 Task runtime。
    pub async fn submit_workspace_chat(
        &self,
        session_id: &SessionId,
        workspace_id: &magi_core::WorkspaceId,
        workspace_path: &Path,
        text: &str,
        request_id: &str,
        user_message_id: &str,
    ) -> Result<SessionTurnResponseDto, crate::errors::ApiError> {
        self.provider.begin_timing();
        let response = TurnService::new(self.state.clone())
            .submit(SessionTurnRequestDto {
                desktop_browser_tools_allowed: false,
                session_id: Some(session_id.to_string()),
                scope: SessionScopeKindDto::Workspace,
                workspace_id: Some(workspace_id.to_string()),
                workspace_path: Some(workspace_path.display().to_string()),
                text: Some(text.to_string()),
                skill_name: None,
                locale: Some("zh-CN".to_string()),
                goal_mode: false,
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
            .await;
        if response.is_ok() {
            self.provider.timing.mark_accepted_returned();
        }
        response
    }

    pub async fn submit_workspace_task_with_access_profile(
        &self,
        session_id: &SessionId,
        workspace_id: &magi_core::WorkspaceId,
        workspace_path: &Path,
        text: &str,
        request_id: &str,
        user_message_id: &str,
        access_profile: Option<AccessProfile>,
    ) -> Result<SessionTurnResponseDto, crate::errors::ApiError> {
        self.provider.begin_timing();
        let response = TurnService::new(self.state.clone())
            .submit(SessionTurnRequestDto {
                desktop_browser_tools_allowed: false,
                session_id: Some(session_id.to_string()),
                scope: SessionScopeKindDto::Workspace,
                workspace_id: Some(workspace_id.to_string()),
                workspace_path: Some(workspace_path.display().to_string()),
                text: Some(text.to_string()),
                skill_name: None,
                locale: Some("zh-CN".to_string()),
                goal_mode: false,
                images: Vec::new(),
                context_references: Vec::new(),
                browser_annotation_refs: Vec::new(),
                browser_node_selections: Vec::new(),
                access_profile,
                orchestrator_session_config: None,
                request_id: Some(request_id.to_string()),
                user_message_id: Some(user_message_id.to_string()),
                placeholder_message_id: None,
                steer_current_turn: false,
                expected_turn_id: None,
                replace_turn_id: None,
            })
            .await;
        if response.is_ok() {
            self.provider.timing.mark_accepted_returned();
        }
        response
    }

    pub async fn steer(
        &self,
        session_id: &SessionId,
        expected_turn_id: &str,
        text: &str,
        request_id: &str,
        user_message_id: &str,
    ) -> Result<SessionTurnResponseDto, crate::errors::ApiError> {
        TurnService::new(self.state.clone())
            .submit(SessionTurnRequestDto {
                desktop_browser_tools_allowed: false,
                session_id: Some(session_id.to_string()),
                scope: SessionScopeKindDto::Personal,
                workspace_id: None,
                workspace_path: None,
                text: Some(text.to_string()),
                skill_name: None,
                locale: Some("zh-CN".to_string()),
                goal_mode: false,
                images: Vec::new(),
                context_references: Vec::new(),
                browser_annotation_refs: Vec::new(),
                browser_node_selections: Vec::new(),
                access_profile: None,
                orchestrator_session_config: None,
                request_id: Some(request_id.to_string()),
                user_message_id: Some(user_message_id.to_string()),
                placeholder_message_id: None,
                steer_current_turn: true,
                expected_turn_id: Some(expected_turn_id.to_string()),
                replace_turn_id: None,
            })
            .await
    }

    pub async fn cancel(&self, session_id: &SessionId) -> Result<(), crate::errors::ApiError> {
        self.cancel_with_workspace(session_id, None).await
    }

    pub async fn cancel_with_workspace(
        &self,
        session_id: &SessionId,
        workspace_id: Option<&magi_core::WorkspaceId>,
    ) -> Result<(), crate::errors::ApiError> {
        crate::routes::sessions::interrupt_session_turn_for_browser_takeover(
            &self.state,
            session_id,
            workspace_id,
        )
        .await
    }

    pub async fn wait_for_terminal(&self, session_id: &SessionId, turn_id: &str) -> CanonicalTurn {
        for _ in 0..1_000 {
            if let Some(turn) = self
                .state
                .session_store
                .canonical_turn_for_session_turn_id(session_id, turn_id)
            {
                if turn.status.is_terminal() {
                    self.provider.timing.mark_terminal_observed();
                    return turn;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("Turn {turn_id} 未在测试窗口内进入终态");
    }

    pub async fn wait_for_first_stream_event(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Vec<EventEnvelope> {
        for _ in 0..1_000 {
            let events = self.events_for(session_id);
            if events.iter().any(|event| {
                event.event_type == "session.turn.item"
                    && event
                        .payload
                        .get("turn_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(turn_id)
                    && event.payload.get("delta").is_some()
            }) {
                if let Some(event) = events.iter().find(|event| {
                    event.event_type == "session.turn.item"
                        && event
                            .payload
                            .get("turn_id")
                            .and_then(serde_json::Value::as_str)
                            == Some(turn_id)
                        && event.payload.get("delta").is_some()
                }) {
                    self.provider.timing.mark_first_stream_event(event.sequence);
                }
                return events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("Turn {turn_id} 未在测试窗口内发布首个流式事件");
    }

    pub async fn wait_for_task_terminal(&self, task_id: &magi_core::TaskId) -> magi_core::Task {
        for _ in 0..6_000 {
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
        let events = self
            .state
            .event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .filter(|event| event.session_id.as_ref() == Some(session_id))
            .collect::<Vec<_>>();
        events
    }
}

#[cfg(test)]
fn git_fixture_command(path: &Path, args: &[&str]) {
    let output = magi_process::std_command("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .expect("Git harness fixture command should start");
    assert!(
        output.status.success(),
        "Git harness fixture command {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(test)]
fn git_fixture_command_expect_failure(path: &Path, args: &[&str]) {
    let output = magi_process::std_command("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .expect("Git harness conflict command should start");
    assert!(
        !output.status.success(),
        "Git harness conflict command {:?} should fail",
        args
    );
}

#[cfg(test)]
fn register_git_workspace(harness: &MagiTurnHarness) -> (magi_core::WorkspaceId, PathBuf) {
    static GIT_FIXTURE_SEQUENCE: OnceLock<AtomicU64> = OnceLock::new();
    let sequence = GIT_FIXTURE_SEQUENCE
        .get_or_init(|| AtomicU64::new(0))
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "magi-turn-harness-git-{}-{}-{}",
        std::process::id(),
        UtcMillis::now().0,
        sequence
    ));
    fs::create_dir_all(&root).expect("Git harness workspace should create");
    git_fixture_command(&root, &["init", "-b", "main"]);
    git_fixture_command(&root, &["config", "user.name", "Magi Harness"]);
    git_fixture_command(
        &root,
        &["config", "user.email", "magi-harness@example.test"],
    );
    fs::write(root.join("README.md"), "base\n").expect("Git harness fixture file should write");
    git_fixture_command(&root, &["add", "README.md"]);
    git_fixture_command(&root, &["commit", "-m", "base"]);
    let workspace_id = magi_core::WorkspaceId::new(format!(
        "workspace-turn-harness-git-{}-{}",
        UtcMillis::now().0,
        sequence
    ));
    harness
        .state
        .workspace_registry
        .register_native_path(workspace_id.clone(), root.clone())
        .expect("Git harness workspace should register");
    (workspace_id, root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use magi_session_store::CanonicalTurnStatus;

    async fn resolve_tool_approval_via_http(
        harness: &MagiTurnHarness,
        session_id: &SessionId,
        workspace_id: &magi_core::WorkspaceId,
        workspace_path: &Path,
        approval_id: &str,
        decision: &str,
    ) {
        let response = crate::routes::build_router(harness.state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/session/tool-approval")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": session_id,
                            "workspaceId": workspace_id,
                            "workspacePath": workspace_path.display().to_string(),
                            "approvalId": approval_id,
                            "decision": decision,
                        })
                        .to_string(),
                    ))
                    .expect("approval request should build"),
            )
            .await
            .expect("approval route should respond");
        let status = response.status();
        if status != StatusCode::OK {
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("approval error body should read");
            panic!(
                "approval route should respond 200, got {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
    }

    async fn wait_for_pending_tool_approval(
        harness: &MagiTurnHarness,
        session_id: &SessionId,
    ) -> magi_conversation_runtime::PendingToolApproval {
        for _ in 0..200 {
            if let Some(pending) = harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(session_id)
                .into_iter()
                .next()
            {
                // Pending 状态先于事件总线发布；等待两者同时可见，避免测试在
                // 审批注册完成但 `tool.approval.requested` 尚未发布的窄窗口读取。
                if harness
                    .events_for(session_id)
                    .iter()
                    .any(|event| event.event_type == "tool.approval.requested")
                {
                    return pending;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("Turn {session_id} 未在测试窗口内产生工具审批请求");
    }

    fn non_classifier_provider_request_count(harness: &MagiTurnHarness) -> usize {
        harness
            .provider
            .requests()
            .into_iter()
            .filter(|request| !request.prompt.contains("Session Turn 编排分类器"))
            .count()
    }

    fn timing_metric(
        timing: &HarnessTimingSnapshot,
        metric: fn(&HarnessTimingSnapshot) -> Option<u128>,
    ) -> u128 {
        metric(timing).expect("性能基准每一轮都必须产生完整时序埋点")
    }

    fn percentile(samples: &[u128], percentile: usize) -> u128 {
        assert!(!samples.is_empty(), "性能基准至少需要一轮样本");
        assert!((1..=100).contains(&percentile));
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let rank = (sorted.len() * percentile).div_ceil(100).max(1) - 1;
        sorted[rank]
    }

    fn print_local_mock_baseline(name: &str, timings: &[HarnessTimingSnapshot]) {
        let accepted = timings
            .iter()
            .map(|timing| timing_metric(timing, |timing| timing.accepted_returned_ms))
            .collect::<Vec<_>>();
        let first_delta = timings
            .iter()
            .map(|timing| timing_metric(timing, |timing| timing.provider_first_delta_ms))
            .collect::<Vec<_>>();
        let first_event = timings
            .iter()
            .map(|timing| timing_metric(timing, |timing| timing.first_stream_event_ms))
            .collect::<Vec<_>>();
        let terminal = timings
            .iter()
            .map(|timing| timing_metric(timing, |timing| timing.terminal_observed_ms))
            .collect::<Vec<_>>();
        assert!(timings.iter().all(|timing| {
            timing.accepted_returned_ms <= timing.provider_first_delta_ms
                && timing.provider_first_delta_ms <= timing.first_stream_event_ms
                && timing.first_stream_event_ms <= timing.terminal_observed_ms
                && timing
                    .first_stream_event_sequence
                    .is_some_and(|sequence| sequence > 0)
        }));
        eprintln!(
            "LOCAL_MOCK_P95 scenario={name} samples={} accepted_ms={{p50:{},p95:{},max:{}}} first_delta_ms={{p50:{},p95:{},max:{}}} first_event_ms={{p50:{},p95:{},max:{}}} terminal_ms={{p50:{},p95:{},max:{}}}",
            timings.len(),
            percentile(&accepted, 50),
            percentile(&accepted, 95),
            accepted.iter().copied().max().unwrap_or_default(),
            percentile(&first_delta, 50),
            percentile(&first_delta, 95),
            first_delta.iter().copied().max().unwrap_or_default(),
            percentile(&first_event, 50),
            percentile(&first_event, 95),
            first_event.iter().copied().max().unwrap_or_default(),
            percentile(&terminal, 50),
            percentile(&terminal, 95),
            terminal.iter().copied().max().unwrap_or_default(),
        );
    }

    #[tokio::test]
    #[ignore = "本地性能基准需显式运行，避免占用常规单元测试"]
    async fn local_mock_provider_five_scenario_p50_p95_baseline() {
        const SAMPLE_COUNT: usize = 20;
        let mut scenario_timings = Vec::new();

        for index in 0..SAMPLE_COUNT {
            let harness = MagiTurnHarness::new("本地 mock 新会话响应");
            let response = harness
                .submit(
                    None,
                    "新建个人普通会话",
                    &format!("local-mock-new-{index}"),
                    &format!("local-mock-new-user-{index}"),
                )
                .await
                .expect("新建个人普通会话应被接纳");
            let session_id = SessionId::new(response.session_id.clone());
            let turn_id = response.turn_id.expect("新建个人普通会话应有 Turn");
            harness
                .wait_for_first_stream_event(&session_id, &turn_id)
                .await;
            harness.wait_for_terminal(&session_id, &turn_id).await;
            scenario_timings.push(harness.provider.timing());
        }
        print_local_mock_baseline("new_personal_chat", &scenario_timings);
        scenario_timings.clear();

        let long_personal = MagiTurnHarness::new("本地 mock 长历史响应");
        let first = long_personal
            .submit(
                None,
                "建立长历史普通会话",
                "local-mock-history-0",
                "local-mock-history-user-0",
            )
            .await
            .expect("长历史首轮应被接纳");
        let history_session_id = SessionId::new(first.session_id.clone());
        let first_turn_id = first.turn_id.expect("长历史首轮应有 Turn");
        long_personal
            .wait_for_first_stream_event(&history_session_id, &first_turn_id)
            .await;
        long_personal
            .wait_for_terminal(&history_session_id, &first_turn_id)
            .await;
        scenario_timings.push(long_personal.provider.timing());
        for index in 1..SAMPLE_COUNT {
            let response = long_personal
                .submit(
                    Some(&history_session_id),
                    &format!("长历史普通会话第 {index} 轮"),
                    &format!("local-mock-history-{index}"),
                    &format!("local-mock-history-user-{index}"),
                )
                .await
                .expect("长历史普通会话应被接纳");
            let turn_id = response.turn_id.expect("长历史普通会话应有 Turn");
            long_personal
                .wait_for_first_stream_event(&history_session_id, &turn_id)
                .await;
            long_personal
                .wait_for_terminal(&history_session_id, &turn_id)
                .await;
            scenario_timings.push(long_personal.provider.timing());
        }
        print_local_mock_baseline("existing_personal_long_history", &scenario_timings);
        scenario_timings.clear();

        for index in 0..SAMPLE_COUNT {
            let harness = MagiTurnHarness::new("本地 mock 工作区聊天响应");
            let workspace_root = tempfile::tempdir().expect("工作区基准目录应创建");
            let workspace_id =
                magi_core::WorkspaceId::new(format!("local-mock-chat-workspace-{index}"));
            harness
                .state
                .workspace_registry
                .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
                .expect("工作区基准路径应注册");
            let workspace_session_id = SessionId::new(format!("local-mock-chat-session-{index}"));
            harness
                .state
                .session_store
                .create_session_for_workspace(
                    workspace_session_id.clone(),
                    "工作区纯聊天基准",
                    Some(workspace_id.to_string()),
                )
                .expect("工作区聊天会话应创建");
            let response = harness
                .submit_workspace_chat(
                    &workspace_session_id,
                    &workspace_id,
                    workspace_root.path(),
                    &format!("工作区纯聊天第 {index} 轮"),
                    &format!("local-mock-workspace-chat-{index}"),
                    &format!("local-mock-workspace-chat-user-{index}"),
                )
                .await
                .expect("工作区纯聊天应被接纳");
            let turn_id = response.turn_id.expect("工作区纯聊天应有 Turn");
            harness
                .wait_for_first_stream_event(&workspace_session_id, &turn_id)
                .await;
            harness
                .wait_for_terminal(&workspace_session_id, &turn_id)
                .await;
            scenario_timings.push(harness.provider.timing());
        }
        print_local_mock_baseline("workspace_plain_chat", &scenario_timings);
        scenario_timings.clear();

        for index in 0..SAMPLE_COUNT {
            let harness = MagiTurnHarness::new_task("本地 mock 工作区工具响应");
            let tool_root = tempfile::tempdir().expect("工作区工具基准目录应创建");
            let tool_workspace_id =
                magi_core::WorkspaceId::new(format!("local-mock-tool-workspace-{index}"));
            harness
                .state
                .workspace_registry
                .register_native_path(tool_workspace_id.clone(), tool_root.path().to_path_buf())
                .expect("工作区工具基准路径应注册");
            let tool_session_id = SessionId::new(format!("local-mock-tool-session-{index}"));
            harness
                .state
                .session_store
                .create_session_for_workspace(
                    tool_session_id.clone(),
                    "工作区工具调用基准",
                    Some(tool_workspace_id.to_string()),
                )
                .expect("工作区工具会话应创建");
            harness.provider.set_tool_then_completed(
                "tool_catalog",
                r#"{"include_external":false}"#,
                "本地 mock 工作区工具响应",
            );
            let response = harness
                .submit_workspace_task(
                    &tool_session_id,
                    &tool_workspace_id,
                    tool_root.path(),
                    &format!("执行工作区工具基准第 {index} 轮"),
                    &format!("local-mock-workspace-tool-{index}"),
                    &format!("local-mock-workspace-tool-user-{index}"),
                )
                .await
                .expect("工作区工具任务应被接纳");
            let turn_id = response.turn_id.expect("工作区工具任务应有 Turn");
            let task_id = response.root_task_id.expect("工作区工具任务应有 root task");
            harness
                .wait_for_task_terminal(&magi_core::TaskId::new(task_id))
                .await;
            harness
                .wait_for_first_stream_event(&tool_session_id, &turn_id)
                .await;
            harness.wait_for_terminal(&tool_session_id, &turn_id).await;
            scenario_timings.push(harness.provider.timing());
        }
        print_local_mock_baseline("workspace_tool", &scenario_timings);
        scenario_timings.clear();

        for index in 0..SAMPLE_COUNT {
            let harness = MagiTurnHarness::new_task("验收子代理1：子代理完成");
            harness
                .provider
                .set_agent_spawn_then_wait("验收子代理1：子代理完成");
            let response = harness
                .submit_task(
                    "请派发一个子代理完成独立验收，再汇总它的结果",
                    &format!("local-mock-subagent-{index}"),
                    &format!("local-mock-subagent-user-{index}"),
                )
                .await
                .expect("子代理基准任务应被接纳");
            let session_id = SessionId::new(response.session_id.clone());
            let turn_id = response.turn_id.expect("子代理基准任务应有 Turn");
            let task_id = response.root_task_id.expect("子代理基准任务应有 root task");
            harness
                .wait_for_task_terminal(&magi_core::TaskId::new(task_id))
                .await;
            harness
                .wait_for_first_stream_event(&session_id, &turn_id)
                .await;
            harness.wait_for_terminal(&session_id, &turn_id).await;
            scenario_timings.push(harness.provider.timing());
        }
        print_local_mock_baseline("primary_with_subagent", &scenario_timings);
    }

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
        let stream_events = harness
            .wait_for_first_stream_event(&session_id, &turn_id)
            .await
            .into_iter()
            .filter(|event| event.event_type == "session.turn.item")
            .filter(|event| event.payload.get("delta").is_some())
            .collect::<Vec<_>>();
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        let (task_count, runner_count, snapshot_created, git_context_created) =
            harness.conversation_runtime_resource_state(&session_id, None);
        assert_eq!(task_count, 0, "普通 Chat 不得创建 TaskStore 任务记录");
        assert_eq!(runner_count, 0, "普通 Chat 不得启动 Runner");
        assert!(!snapshot_created, "普通 Chat 不得物化 Snapshot session");
        assert!(
            !git_context_created,
            "普通 Chat 不得创建 Git execution context"
        );
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
        assert!(
            !stream_events.is_empty(),
            "应通过真实 EventBus 发布流式事件"
        );
        assert!(
            stream_events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        let timing = harness.provider.timing();
        assert!(timing.accepted_returned_ms.is_some());
        assert!(timing.provider_request_started_ms.is_some());
        assert!(timing.provider_first_delta_ms.is_some());
        assert!(timing.first_stream_event_ms.is_some());
        assert!(
            timing
                .first_stream_event_sequence
                .is_some_and(|sequence| sequence > 0)
        );
        assert!(timing.terminal_observed_ms.is_some());
        assert!(timing.provider_request_started_ms <= timing.provider_first_delta_ms);
        assert!(timing.provider_first_delta_ms <= timing.first_stream_event_ms);
        assert!(timing.first_stream_event_ms <= timing.terminal_observed_ms);
    }

    #[tokio::test]
    async fn workspace_conversation_does_not_create_task_runtime_resources() {
        let harness = MagiTurnHarness::new("工作区普通聊天成功");
        let workspace_root = tempfile::tempdir().expect("workspace chat root should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-conversation-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("workspace chat root should register");
        let session_id = SessionId::new("harness-conversation-workspace-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "工作区普通聊天",
                Some(workspace_id.to_string()),
            )
            .expect("workspace chat session should create");

        let response = harness
            .submit_workspace_chat(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "只进行普通工作区聊天，不执行工具",
                "harness-workspace-chat-request",
                "harness-workspace-chat-user",
            )
            .await
            .expect("workspace Conversation Turn 应被接纳");
        assert_eq!(response.route, crate::dto::SessionTurnRouteDto::Chat);
        assert_eq!(
            response.execution_profile,
            Some(magi_conversation_runtime::ExecutionProfile::Conversation)
        );
        let turn_id = response
            .turn_id
            .clone()
            .expect("workspace chat should have turn");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item.content.as_deref() == Some("工作区普通聊天成功")
        }));

        let (task_count, runner_count, snapshot_created, git_context_created) =
            harness.conversation_runtime_resource_state(&session_id, Some(workspace_root.path()));
        assert_eq!(task_count, 0, "工作区普通 Chat 不得创建 TaskStore 任务记录");
        assert_eq!(runner_count, 0, "工作区普通 Chat 不得启动 Runner");
        assert!(
            !snapshot_created,
            "工作区普通 Chat 不得物化 Snapshot session"
        );
        assert!(
            !git_context_created,
            "工作区普通 Chat 不得创建 Git execution context"
        );
        assert!(harness.state.task_store().is_none());
        assert!(harness.state.runner_manager().is_none());
        assert_eq!(harness.provider.requests().len(), 1);
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
    async fn task_profile_permission_block_stops_write_tool_without_provider_loop() {
        let harness = MagiTurnHarness::new_task("不会执行受限写入");
        let workspace_root = tempfile::tempdir().expect("permission workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-permission-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("permission workspace should register");
        let session_id = SessionId::new("harness-permission-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "权限阻塞验收",
                Some(workspace_id.to_string()),
            )
            .expect("permission session should create");
        let target = workspace_root.path().join("should-not-write.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf blocked > {}", target.display())
            })
            .to_string(),
            "不会执行受限写入",
        );
        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "执行一个任务：写入文件并汇总结果",
                "harness-permission-request",
                "harness-permission-user",
                Some(AccessProfile::ReadOnly),
            )
            .await
            .expect("permission-blocked task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("permission task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("permission task should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Failed);
        assert_eq!(task.status, magi_core::TaskStatus::Failed);
        assert!(!target.exists(), "只读访问模式下写工具不得产生文件副作用");
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item
                    .tool
                    .as_ref()
                    .is_some_and(|tool| tool.name == "shell_exec")
        }));
        assert_eq!(harness.provider.requests().len(), 1);
    }

    #[tokio::test]
    async fn read_only_profile_rejects_explicit_file_write_without_approval_or_side_effect() {
        let harness = MagiTurnHarness::new_task("只读模式拒绝显式文件写入");
        let workspace_root = tempfile::tempdir().expect("read-only workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-read-only-file-write-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("read-only workspace should register");
        let session_id = SessionId::new("harness-read-only-file-write-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "只读文件写入验收",
                Some(workspace_id.to_string()),
            )
            .expect("read-only session should create");
        let target = workspace_root.path().join("read-only-write.txt");
        harness.provider.set_tool_then_completed(
            "file_write",
            serde_json::json!({
                "path": target.display().to_string(),
                "content": "must not write"
            })
            .to_string(),
            "只读模式不会写入文件",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "执行一个任务：调用 file_write 写入工作区文件，然后汇总结果",
                "harness-read-only-file-write-request",
                "harness-read-only-file-write-user",
                Some(AccessProfile::ReadOnly),
            )
            .await
            .expect("read-only file write task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("read-only task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("read-only task should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Failed);
        assert_eq!(task.status, magi_core::TaskStatus::Failed);
        assert!(!target.exists(), "只读访问模式下写工具不得产生文件副作用");
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            0,
            "ReadOnly 隐藏 file_write 后应在任务模型调用前直接失败，不能进入 Provider 重试循环"
        );
        assert_eq!(
            harness.provider.requests().len(),
            0,
            "ReadOnly 隐藏 file_write 后不应进入 Provider 请求"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested")
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item.status == magi_session_store::CanonicalTurnItemStatus::Failed
                && item.content.as_deref().is_some_and(|content| {
                    content.contains("要求调用工具 file_write")
                        && content.contains("当前工具面没有暴露该工具")
                })
        }));
    }

    #[tokio::test]
    async fn read_only_profile_allows_file_read_without_approval() {
        let harness = MagiTurnHarness::new_task("只读模式读取文件完成");
        let workspace_root = tempfile::tempdir().expect("read-only read workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-read-only-file-read-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("read-only read workspace should register");
        let session_id = SessionId::new("harness-read-only-file-read-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "只读文件读取验收",
                Some(workspace_id.to_string()),
            )
            .expect("read-only read session should create");
        let target = workspace_root.path().join("read-only-read.txt");
        fs::write(&target, "read-only content").expect("read-only fixture should write");
        harness.provider.set_tool_then_completed(
            "file_read",
            serde_json::json!({"path": target.display().to_string()}).to_string(),
            "只读模式读取完成",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "在只读模式下调用 file_read 读取工作区文件，然后汇总结果",
                "harness-read-only-file-read-request",
                "harness-read-only-file-read-user",
                Some(AccessProfile::ReadOnly),
            )
            .await
            .expect("read-only file read task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("read-only file read task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("read-only file read task should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;

        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(fs::read_to_string(&target).unwrap(), "read-only content");
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "只读 file_read 应执行读取轮并请求一次最终答复"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "只读 file_read 不应进入审批流程"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "file_read" && tool.result.is_some() && tool.error.is_none()
                })
        }));
    }

    #[tokio::test]
    async fn full_access_profile_executes_write_tool_without_approval() {
        let harness = MagiTurnHarness::new_task("完全授权写入完成");
        let workspace_root = tempfile::tempdir().expect("full access workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-full-access-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("full access workspace should register");
        let session_id = SessionId::new("harness-full-access-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "完全授权写入验收",
                Some(workspace_id.to_string()),
            )
            .expect("full access session should create");
        let target = workspace_root.path().join("full-access.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf full_access > {}", target.display())
            })
            .to_string(),
            "完全授权后的任务已完成",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 执行一个完全授权的写入命令",
                "harness-full-access-request",
                "harness-full-access-user",
                Some(AccessProfile::FullAccess),
            )
            .await
            .expect("full access task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("full access task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("full access task should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;

        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(&target).expect("full access write should be readable"),
            "full_access"
        );
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty(),
            "完全授权写入不得产生 pending 审批"
        );
        assert!(
            !harness
                .events_for(&session_id)
                .iter()
                .any(|event| event.event_type == "tool.approval.requested"),
            "完全授权写入不得发布审批请求"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "shell_exec" && tool.result.is_some() && tool.error.is_none()
                })
        }));
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "完全授权写入应执行工具轮并请求一次最终答复"
        );
    }

    #[tokio::test]
    async fn restricted_profile_approval_allows_original_write_tool_once() {
        let harness = MagiTurnHarness::new_task("审批后完成");
        let workspace_root = tempfile::tempdir().expect("approval workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-approval-allow-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("approval workspace should register");
        let session_id = SessionId::new("harness-approval-allow-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "权限审批允许验收",
                Some(workspace_id.to_string()),
            )
            .expect("approval session should create");
        let target = workspace_root.path().join("approval-allowed.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf approved > {}", target.display())
            })
            .to_string(),
            "权限审批后的任务已完成",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 执行一个命令并在审批后汇总结果",
                "harness-approval-allow-request",
                "harness-approval-allow-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("restricted approval task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("approval task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("approval task should have root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "shell_exec");
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .any(|event| event.event_type == "tool.approval.requested"),
            "真实工具执行必须发布审批请求事件"
        );

        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            workspace_root.path(),
            &pending.approval_id,
            "allow_once",
        )
        .await;

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(&target).expect("approved write should be readable"),
            "approved"
        );
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty(),
            "审批允许后不得遗留 pending 请求"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .any(|event| event.event_type == "tool.approval.resolved"),
            "真实审批路由必须发布 resolved 事件"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "允许原始调用后只应有工具轮和一次最终答复轮"
        );
    }

    #[tokio::test]
    async fn restricted_profile_approval_denial_fails_without_repeating_write_tool() {
        let harness = MagiTurnHarness::new_task("不会执行被拒绝写入");
        let workspace_root = tempfile::tempdir().expect("approval workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-approval-deny-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("approval workspace should register");
        let session_id = SessionId::new("harness-approval-deny-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "权限审批拒绝验收",
                Some(workspace_id.to_string()),
            )
            .expect("approval session should create");
        let target = workspace_root.path().join("approval-denied.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf denied > {}", target.display())
            })
            .to_string(),
            "不会执行被拒绝写入",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 执行一个命令但必须等待用户审批",
                "harness-approval-deny-request",
                "harness-approval-deny-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("restricted approval task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("approval task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("approval task should have root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            workspace_root.path(),
            &pending.approval_id,
            "deny",
        )
        .await;

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Failed);
        assert_eq!(task.status, magi_core::TaskStatus::Failed);
        assert!(!target.exists(), "用户拒绝后不得产生写入副作用");
        assert!(
            turn.items.iter().any(|item| {
                item.kind == CanonicalTurnItemKind::ToolCall
                    && item.tool.as_ref().is_some_and(|tool| {
                        tool.name == "shell_exec"
                            && tool
                                .error
                                .as_deref()
                                .is_some_and(|error| error.contains("拒绝"))
                    })
            }),
            "拒绝结果必须写入 canonical ToolCall"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            1,
            "拒绝不可重试的写工具后不得再次请求 Provider"
        );
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty(),
            "审批拒绝后不得遗留 pending 请求"
        );
    }

    #[tokio::test]
    async fn restricted_profile_pending_approval_is_cancelled_with_turn_without_side_effect() {
        let harness = MagiTurnHarness::new_task("取消待审批写入");
        let workspace_root =
            tempfile::tempdir().expect("approval cancellation workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-approval-cancel-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("approval cancellation workspace should register");
        let session_id = SessionId::new("harness-approval-cancel-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "取消待审批写入验收",
                Some(workspace_id.to_string()),
            )
            .expect("approval cancellation session should create");
        let target = workspace_root.path().join("approval-cancelled.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf cancelled > {}", target.display())
            })
            .to_string(),
            "不会执行取消的写入",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 执行一个命令，但在审批前取消当前 Turn",
                "harness-approval-cancel-request",
                "harness-approval-cancel-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("approval cancellation task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("approval cancellation task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("approval cancellation task should have root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "shell_exec");

        harness
            .cancel_with_workspace(&session_id, Some(&workspace_id))
            .await
            .expect("取消待审批 Turn 应成功");

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Cancelled);
        assert!(
            matches!(
                task.status,
                magi_core::TaskStatus::Failed | magi_core::TaskStatus::Killed
            ),
            "取消待审批 Turn 后任务必须收口为失败或终止，实际为 {:?}",
            task.status
        );
        assert!(!target.exists(), "取消待审批操作不得产生文件副作用");
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty(),
            "取消 Turn 后不得遗留 pending 审批"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            1,
            "取消待审批操作后不得重新请求 Provider"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.resolved"),
            "未作出决定的审批取消不得发布 resolved 事件"
        );
    }

    #[tokio::test]
    async fn restricted_profile_expired_approval_rejects_without_side_effect() {
        let harness = MagiTurnHarness::new_task("过期审批拒绝写入");
        let workspace_root = tempfile::tempdir().expect("approval expiry workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-approval-expiry-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("approval expiry workspace should register");
        let session_id = SessionId::new("harness-approval-expiry-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "过期审批拒绝验收",
                Some(workspace_id.to_string()),
            )
            .expect("approval expiry session should create");
        let target = workspace_root.path().join("approval-expired.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf expired > {}", target.display())
            })
            .to_string(),
            "过期审批不应执行写入",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 执行一个命令，但审批过期后不得执行",
                "harness-approval-expiry-request",
                "harness-approval-expiry-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("approval expiry task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("approval expiry task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("approval expiry task should have root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "shell_exec");
        assert_eq!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .expire_stale(UtcMillis(UtcMillis::now().0 + TOOL_APPROVAL_TTL_MILLIS + 1,)),
            1,
            "测试必须显式推进审批时钟并清理 pending 请求"
        );

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Failed);
        assert_eq!(task.status, magi_core::TaskStatus::Failed);
        assert!(!target.exists(), "过期审批不得产生文件副作用");
        assert!(
            turn.items.iter().any(|item| {
                item.kind == CanonicalTurnItemKind::ToolCall
                    && item.tool.as_ref().is_some_and(|tool| {
                        tool.name == "shell_exec"
                            && tool.error.as_deref().is_some_and(|error| {
                                error.contains("tool_approval_expired") || error.contains("过期")
                            })
                    })
            }),
            "canonical ToolCall 必须记录审批过期错误"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .any(|event| event.event_type == "tool.approval.requested")
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.resolved"),
            "审批过期不得伪造 resolved 事件"
        );
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty()
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            1,
            "审批过期后不得重复请求 Provider"
        );
    }

    #[tokio::test]
    async fn restricted_profile_file_remove_denial_preserves_path_without_retry() {
        let harness = MagiTurnHarness::new_task("拒绝删除后收口");
        let workspace_root = tempfile::tempdir().expect("file remove workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-file-remove-deny-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("file remove workspace should register");
        let session_id = SessionId::new("harness-file-remove-deny-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "拒绝删除验收",
                Some(workspace_id.to_string()),
            )
            .expect("file remove session should create");
        let target = workspace_root.path().join("preserve-me.txt");
        fs::write(&target, "keep").expect("file remove fixture should write");
        harness.provider.set_tool_then_completed(
            "file_remove",
            serde_json::json!({ "path": target.display().to_string() }).to_string(),
            "删除被拒绝",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 file_remove 删除文件，但等待用户审批",
                "harness-file-remove-deny-request",
                "harness-file-remove-deny-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("file remove task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("file remove task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("file remove task should have root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "file_remove");
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            workspace_root.path(),
            &pending.approval_id,
            "deny",
        )
        .await;

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Failed);
        assert_eq!(task.status, magi_core::TaskStatus::Failed);
        assert!(target.exists(), "拒绝 file_remove 后目标文件必须保留");
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            1,
            "拒绝 file_remove 后不得重复请求 Provider"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "file_remove"
                        && tool
                            .error
                            .as_deref()
                            .is_some_and(|error| error.contains("拒绝"))
                })
        }));
    }

    #[tokio::test]
    async fn restricted_profile_auto_allows_file_patch_and_file_mkdir() {
        let patch_harness = MagiTurnHarness::new_task("file_patch 自动允许后完成");
        let patch_workspace_root = tempfile::tempdir().expect("file patch workspace should create");
        let patch_workspace_id = magi_core::WorkspaceId::new("harness-file-patch-workspace");
        patch_harness
            .state
            .workspace_registry
            .register_native_path(
                patch_workspace_id.clone(),
                patch_workspace_root.path().to_path_buf(),
            )
            .expect("file patch workspace should register");
        let patch_session_id = SessionId::new("harness-file-patch-session");
        patch_harness
            .state
            .session_store
            .create_session_for_workspace(
                patch_session_id.clone(),
                "file_patch 自动允许验收",
                Some(patch_workspace_id.to_string()),
            )
            .expect("file patch session should create");
        let patch_target = patch_workspace_root.path().join("patch-target.txt");
        fs::write(&patch_target, "before\n").expect("file patch fixture should write");
        patch_harness.provider.set_tool_then_completed(
            "file_patch",
            serde_json::json!({
                "path": patch_target.display().to_string(),
                "old_string": "before",
                "new_string": "after"
            })
            .to_string(),
            "file_patch 自动允许后完成",
        );

        let patch_response = patch_harness
            .submit_workspace_task_with_access_profile(
                &patch_session_id,
                &patch_workspace_id,
                patch_workspace_root.path(),
                "调用 file_patch 修改工作区内文件",
                "harness-file-patch-request",
                "harness-file-patch-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("file patch task should be accepted");
        let patch_turn_id = patch_response
            .turn_id
            .clone()
            .expect("file patch task should have turn");
        let patch_root_task_id = patch_response
            .root_task_id
            .clone()
            .expect("file patch task should have root task");
        let patch_turn = patch_harness
            .wait_for_terminal(&patch_session_id, &patch_turn_id)
            .await;
        let patch_task = patch_harness
            .wait_for_task_terminal(&magi_core::TaskId::new(patch_root_task_id))
            .await;
        assert_eq!(patch_turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(patch_task.status, magi_core::TaskStatus::Completed);
        assert_eq!(fs::read_to_string(&patch_target).unwrap(), "after\n");
        assert_eq!(
            patch_harness
                .events_for(&patch_session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            0,
            "Restricted 下工作区内 file_patch 应自动允许"
        );
        assert_eq!(
            patch_harness
                .events_for(&patch_session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            0,
            "自动允许的 file_patch 不应伪造审批收口"
        );
        assert_eq!(
            non_classifier_provider_request_count(&patch_harness),
            2,
            "file_patch 允许后应执行工具轮和一次最终答复轮"
        );

        let mkdir_harness = MagiTurnHarness::new_task("file_mkdir 自动允许后完成");
        let mkdir_workspace_root = tempfile::tempdir().expect("file mkdir workspace should create");
        let mkdir_workspace_id = magi_core::WorkspaceId::new("harness-file-mkdir-workspace");
        mkdir_harness
            .state
            .workspace_registry
            .register_native_path(
                mkdir_workspace_id.clone(),
                mkdir_workspace_root.path().to_path_buf(),
            )
            .expect("file mkdir workspace should register");
        let mkdir_session_id = SessionId::new("harness-file-mkdir-session");
        mkdir_harness
            .state
            .session_store
            .create_session_for_workspace(
                mkdir_session_id.clone(),
                "file_mkdir 自动允许验收",
                Some(mkdir_workspace_id.to_string()),
            )
            .expect("file mkdir session should create");
        let mkdir_target = mkdir_workspace_root.path().join("nested").join("created");
        mkdir_harness.provider.set_tool_then_completed(
            "file_mkdir",
            serde_json::json!({"path": mkdir_target.display().to_string()}).to_string(),
            "file_mkdir 自动允许后完成",
        );

        let mkdir_response = mkdir_harness
            .submit_workspace_task_with_access_profile(
                &mkdir_session_id,
                &mkdir_workspace_id,
                mkdir_workspace_root.path(),
                "调用 file_mkdir 创建工作区内目录",
                "harness-file-mkdir-request",
                "harness-file-mkdir-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("file mkdir task should be accepted");
        let mkdir_turn_id = mkdir_response
            .turn_id
            .clone()
            .expect("file mkdir task should have turn");
        let mkdir_root_task_id = mkdir_response
            .root_task_id
            .clone()
            .expect("file mkdir task should have root task");
        let mkdir_turn = mkdir_harness
            .wait_for_terminal(&mkdir_session_id, &mkdir_turn_id)
            .await;
        let mkdir_task = mkdir_harness
            .wait_for_task_terminal(&magi_core::TaskId::new(mkdir_root_task_id))
            .await;
        assert_eq!(mkdir_turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(mkdir_task.status, magi_core::TaskStatus::Completed);
        assert!(
            mkdir_target.is_dir(),
            "Restricted 下工作区内 file_mkdir 应创建目录"
        );
        assert_eq!(
            non_classifier_provider_request_count(&mkdir_harness),
            2,
            "file_mkdir 自动允许后应执行工具轮和一次最终答复轮"
        );
        assert!(mkdir_turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "file_mkdir" && tool.error.is_none() && tool.result.is_some()
                })
        }));
        assert_eq!(
            mkdir_harness
                .events_for(&mkdir_session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            0,
            "Restricted 下工作区内 file_mkdir 应自动允许"
        );
    }

    #[tokio::test]
    async fn restricted_profile_rejects_write_outside_workspace_without_approval() {
        let harness = MagiTurnHarness::new_task("越界写入被拒绝");
        let workspace_root = tempfile::tempdir().expect("workspace should create");
        let outside_root = tempfile::tempdir().expect("outside workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-path-deny-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("workspace should register");
        let session_id = SessionId::new("harness-path-deny-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "越界路径拒绝验收",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let target = outside_root.path().join("escape.txt");
        harness.provider.set_tool_then_completed(
            "file_write",
            serde_json::json!({
                "path": target.display().to_string(),
                "content": "must not escape"
            })
            .to_string(),
            "越界写入不应执行",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 file_write 写入工作区之外的路径",
                "harness-path-deny-request",
                "harness-path-deny-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("path policy task should be accepted");
        let turn_id = response.turn_id.clone().expect("task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("task should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;

        assert_eq!(turn.status, CanonicalTurnStatus::Failed);
        assert_eq!(task.status, magi_core::TaskStatus::Failed);
        assert!(!target.exists(), "工作区外路径不得产生文件副作用");
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty(),
            "路径拒绝不应创建 pending approval"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "确定性的路径拒绝不应发布审批请求"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            1,
            "确定性的路径拒绝后不得重试 Provider"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item
                    .tool
                    .as_ref()
                    .is_some_and(|tool| tool.name == "file_write" && tool.error.is_some())
        }));
    }

    #[tokio::test]
    async fn restricted_profile_allow_for_turn_reuses_write_tool_grant() {
        let harness = MagiTurnHarness::new_task("按 Turn 授权完成");
        let workspace_root = tempfile::tempdir().expect("turn grant workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-turn-grant-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("turn grant workspace should register");
        let session_id = SessionId::new("harness-turn-grant-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "按 Turn 授权验收",
                Some(workspace_id.to_string()),
            )
            .expect("turn grant session should create");
        let target = workspace_root.path().join("turn-grant.txt");
        harness.provider.set_tool_then_completed_twice(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf turn_grant > {}", target.display())
            })
            .to_string(),
            "按 Turn 授权后的任务已完成",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 两次完成同一 Turn 内的写入操作",
                "harness-turn-grant-request",
                "harness-turn-grant-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("turn grant task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("turn grant task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("turn grant task should have root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            workspace_root.path(),
            &pending.approval_id,
            "allow_for_turn",
        )
        .await;

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(&target).expect("turn grant write should be readable"),
            "turn_grant"
        );
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty(),
            "按 Turn 授权完成后不得遗留 pending 请求"
        );
        let approval_requests = harness
            .events_for(&session_id)
            .into_iter()
            .filter(|event| event.event_type == "tool.approval.requested")
            .count();
        assert_eq!(approval_requests, 1, "同一 Turn 的同名工具只需审批一次");
        let tool_calls = turn
            .items
            .iter()
            .filter(|item| {
                item.kind == CanonicalTurnItemKind::ToolCall
                    && item
                        .tool
                        .as_ref()
                        .is_some_and(|tool| tool.name == "shell_exec")
            })
            .count();
        assert_eq!(tool_calls, 2, "按 Turn 授权后应执行两次写工具调用");
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            3,
            "两次工具轮后应仅请求一次最终答复"
        );
    }

    #[tokio::test]
    async fn restricted_profile_allow_once_requires_approval_for_each_write_tool_call() {
        let harness = MagiTurnHarness::new_task("逐次授权完成");
        let workspace_root = tempfile::tempdir().expect("allow once workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-allow-once-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("allow once workspace should register");
        let session_id = SessionId::new("harness-allow-once-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "逐次授权验收",
                Some(workspace_id.to_string()),
            )
            .expect("allow once session should create");
        let target = workspace_root.path().join("allow-once.txt");
        harness.provider.set_tool_then_completed_twice(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf allow_once >> {}", target.display())
            })
            .to_string(),
            "逐次授权后的任务已完成",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 两次，每次写入前都要求用户确认",
                "harness-allow-once-request",
                "harness-allow-once-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("allow once task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("allow once task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("allow once task should have root task");

        let first_pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            workspace_root.path(),
            &first_pending.approval_id,
            "allow_once",
        )
        .await;
        let second_pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_ne!(
            first_pending.approval_id, second_pending.approval_id,
            "allow_once 不得把第一次决定提升为 Turn 级授权"
        );
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            workspace_root.path(),
            &second_pending.approval_id,
            "allow_once",
        )
        .await;

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(&target).expect("allow once writes should be readable"),
            "allow_onceallow_once"
        );
        let approval_requests = harness
            .events_for(&session_id)
            .iter()
            .filter(|event| event.event_type == "tool.approval.requested")
            .count();
        let approval_resolutions = harness
            .events_for(&session_id)
            .iter()
            .filter(|event| event.event_type == "tool.approval.resolved")
            .count();
        assert_eq!(approval_requests, 2, "allow_once 的两次调用都必须请求审批");
        assert_eq!(
            approval_resolutions, 2,
            "两次 allow_once 都必须产生 resolved 事件"
        );
        assert_eq!(
            turn.items
                .iter()
                .filter(|item| {
                    item.kind == CanonicalTurnItemKind::ToolCall
                        && item
                            .tool
                            .as_ref()
                            .is_some_and(|tool| tool.name == "shell_exec")
                })
                .count(),
            2,
            "两次 allow_once 都应执行原始写工具"
        );
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty(),
            "逐次授权完成后不得遗留 pending 请求"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            3,
            "两次工具轮后应仅请求一次最终答复"
        );
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
    async fn steer_routes_to_the_active_turn_and_keeps_the_same_turn_identity() {
        let harness = MagiTurnHarness::new("不会先完成");
        harness.provider.set_hold_for_cancellation();
        let initial = harness
            .submit(
                None,
                "请回复一句话并保持等待",
                "harness-steer-root-request",
                "harness-steer-root-user",
            )
            .await
            .expect("可引导的 Turn 应先被接纳");
        let session_id = SessionId::new(initial.session_id.clone());
        let turn_id = initial.turn_id.clone().expect("初始 Turn 应有身份");
        for _ in 0..100 {
            if !harness.provider.requests().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            !harness.provider.requests().is_empty(),
            "steer 前 Provider 必须已经进入执行"
        );
        let steered = harness
            .steer(
                &session_id,
                &turn_id,
                "优先收口当前响应",
                "harness-steer-request",
                "harness-steer-user",
            )
            .await
            .expect("steer 应通过真实 TurnService 接纳");
        assert_eq!(steered.route, crate::dto::SessionTurnRouteDto::Steer);
        assert_eq!(steered.steered_turn_id.as_deref(), Some(turn_id.as_str()));
        assert_eq!(steered.turn_id.as_deref(), Some(turn_id.as_str()));
        let current = harness
            .state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("steer 后仍应保留同一 current Turn");
        assert_eq!(current.turn_id, turn_id);
        assert!(current.items.iter().any(|item| {
            item.kind == "user_message" && item.content.as_deref() == Some("优先收口当前响应")
        }));
        harness
            .cancel(&session_id)
            .await
            .expect("steer 场景应可取消");
        let terminal = harness
            .wait_for_terminal(&session_id, &current.turn_id)
            .await;
        assert_eq!(terminal.status, CanonicalTurnStatus::Cancelled);
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
        let turn_id = "harness-active-turn";
        let attempt = match harness
            .state
            .turn_coordinator()
            .execute_command(
                &session_id,
                TurnCommand::Start(TurnAdmission {
                    turn_id: turn_id.to_string(),
                    request_id: "harness-active-request".to_string(),
                    request_fingerprint: "harness-active-fingerprint".to_string(),
                    profile: ExecutionProfile::Conversation,
                }),
            )
            .expect("排队占用 Turn 应接纳")
        {
            CoordinatorCommandResult::Admission(CoordinatorAdmission::Accepted(attempt)) => attempt,
            other => panic!("unexpected queue fixture admission: {other:?}"),
        };
        harness
            .state
            .turn_event_sink()
            .accept_conversation_turn_with_timeline_entry(
                session_id.clone(),
                None,
                TimelineEntryInput::new(
                    "harness-active-timeline",
                    TimelineEntryKind::UserMessage,
                    "占用执行资源",
                    UtcMillis::now(),
                ),
                ActiveExecutionTurn {
                    turn_id: turn_id.to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "accepted".to_string(),
                    completed_at: None,
                    user_message: Some("占用执行资源".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("排队场景应通过 canonical sink 持久化活跃 Turn");
        harness
            .state
            .turn_coordinator()
            .execute_command(
                &session_id,
                TurnCommand::SetStatus {
                    attempt: attempt.clone(),
                    status: magi_conversation_runtime::CoordinatorTurnStatus::Preparing,
                },
            )
            .expect("排队占用 Turn 应进入 preparing");
        harness
            .state
            .turn_coordinator()
            .execute_command(
                &session_id,
                TurnCommand::SetStatus {
                    attempt,
                    status: magi_conversation_runtime::CoordinatorTurnStatus::Running,
                },
            )
            .expect("排队占用 Turn 应进入 running");
        harness
            .state
            .turn_event_sink()
            .set_status_domain(&session_id, Some(turn_id), "running")
            .expect("排队占用 Turn running 状态应持久化");

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
    async fn workspace_task_with_external_git_branch_drift_fails_before_provider_dispatch() {
        let harness = MagiTurnHarness::new_task("不会在漂移 workspace 中执行");
        let (workspace_id, workspace_root) = register_git_workspace(&harness);
        let session_id = SessionId::new("harness-git-drift-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "Git 漂移阻塞验收",
                Some(workspace_id.to_string()),
            )
            .expect("Git 漂移 harness session should create");

        harness
            .state
            .ensure_session_code_context(&session_id, &Some(workspace_id.clone()))
            .await
            .expect("Git 漂移场景应先建立稳定 context");
        harness
            .state
            .release_session_git_execution_lease(&session_id);
        git_fixture_command(&workspace_root, &["switch", "-c", "external/drift"]);

        let response = harness
            .submit_workspace_task(
                &session_id,
                &workspace_id,
                &workspace_root,
                "执行一个复杂任务：读取当前工作区并汇总结果",
                "harness-git-drift-request",
                "harness-git-drift-user",
            )
            .await
            .expect("Git 漂移请求应先写入 accepted 事实");
        assert_eq!(
            response.execution_profile,
            Some(magi_conversation_runtime::ExecutionProfile::Task)
        );
        let turn_id = response
            .turn_id
            .clone()
            .expect("Git 漂移 Task 应返回 Turn 身份");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Git 漂移 Task 应返回 root task 身份");

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Failed);
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(task.status, magi_core::TaskStatus::Failed);
        assert!(
            task.output_refs
                .iter()
                .any(|message| message.contains("Git context")),
            "Git 漂移必须保留可诊断的 task 阻塞事实: {task:?}"
        );
        assert!(
            harness.provider.requests().is_empty(),
            "Git 前置检查失败时不得向 Provider 派发请求"
        );
        let context = harness
            .state
            .session_code_contexts
            .get(session_id.as_str())
            .expect("Git 漂移后 context 仍应可恢复");
        assert!(context.has_external_drift());

        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn workspace_task_preserves_dirty_git_context_and_can_complete() {
        let harness = MagiTurnHarness::new_task("dirty workspace 完成");
        let (workspace_id, workspace_root) = register_git_workspace(&harness);
        let session_id = SessionId::new("harness-git-dirty-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "Git dirty 验收",
                Some(workspace_id.to_string()),
            )
            .expect("Git dirty harness session should create");
        harness
            .state
            .ensure_session_code_context(&session_id, &Some(workspace_id.clone()))
            .await
            .expect("Git dirty 场景应先建立稳定 context");
        harness
            .state
            .release_session_git_execution_lease(&session_id);
        fs::write(workspace_root.join("README.md"), "dirty change\n")
            .expect("Git dirty file should write");

        let response = harness
            .submit_workspace_task(
                &session_id,
                &workspace_id,
                &workspace_root,
                "执行一个复杂任务：读取当前工作区并汇总结果",
                "harness-git-dirty-request",
                "harness-git-dirty-user",
            )
            .await
            .expect("Git dirty 请求应进入执行链");
        let turn_id = response
            .turn_id
            .clone()
            .expect("Git dirty Task 应返回 Turn 身份");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert!(!harness.provider.requests().is_empty());
        let context = harness
            .state
            .session_code_contexts
            .get(session_id.as_str())
            .expect("Git dirty 完成后 context 应保留");
        assert!(context.git.dirty.has_uncommitted);
        assert!(!context.has_external_drift());

        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn workspace_task_with_git_merge_conflict_fails_before_provider_dispatch() {
        let harness = MagiTurnHarness::new_task("不会在冲突 workspace 中执行");
        let (workspace_id, workspace_root) = register_git_workspace(&harness);
        let session_id = SessionId::new("harness-git-conflict-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "Git 冲突阻塞验收",
                Some(workspace_id.to_string()),
            )
            .expect("Git 冲突 harness session should create");
        harness
            .state
            .ensure_session_code_context(&session_id, &Some(workspace_id.clone()))
            .await
            .expect("Git 冲突场景应先建立稳定 context");
        harness
            .state
            .release_session_git_execution_lease(&session_id);

        git_fixture_command(&workspace_root, &["switch", "-c", "conflicting"]);
        fs::write(workspace_root.join("README.md"), "feature\n")
            .expect("Git conflict feature file should write");
        git_fixture_command(&workspace_root, &["add", "README.md"]);
        git_fixture_command(&workspace_root, &["commit", "-m", "feature change"]);
        git_fixture_command(&workspace_root, &["switch", "main"]);
        fs::write(workspace_root.join("README.md"), "main change\n")
            .expect("Git conflict main file should write");
        git_fixture_command(&workspace_root, &["add", "README.md"]);
        git_fixture_command(&workspace_root, &["commit", "-m", "main change"]);
        git_fixture_command_expect_failure(&workspace_root, &["merge", "conflicting"]);

        let response = harness
            .submit_workspace_task(
                &session_id,
                &workspace_id,
                &workspace_root,
                "执行一个复杂任务：处理当前工作区冲突并汇总结果",
                "harness-git-conflict-request",
                "harness-git-conflict-user",
            )
            .await
            .expect("Git 冲突请求应先写入 accepted 事实");
        let turn_id = response
            .turn_id
            .clone()
            .expect("Git 冲突 Task 应返回 Turn 身份");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Git 冲突 Task 应返回 root task 身份");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Failed);
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(task.status, magi_core::TaskStatus::Failed);
        assert!(
            task.output_refs
                .iter()
                .any(|message| { message.contains("未解决的 Git merge conflict") })
        );
        assert!(
            harness.provider.requests().is_empty(),
            "Git 冲突前置检查失败时不得向 Provider 派发请求"
        );
        let context = harness
            .state
            .session_code_contexts
            .get(session_id.as_str())
            .expect("Git 冲突后 context 仍应可恢复");
        assert_eq!(context.git.dirty.conflicted_paths, vec!["README.md"]);

        let _ = fs::remove_dir_all(workspace_root);
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
