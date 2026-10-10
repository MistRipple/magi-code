//! 基于真实 TurnService 的消息响应链路测试 harness。
//!
//! 该模块只在测试构建中编译。它不绕过 API 或 canonical sink，而是装配与 daemon
//! 相同的 Conversation dispatcher，使用可观测的流式 Provider 代替网络 Provider。

use crate::{
    dto::{SessionScopeKindDto, SessionTurnRequestDto, SessionTurnResponseDto},
    state::{ApiState, RuntimeStatePersistence},
    turn_service::TurnService,
};
use axum::{body::Body, http::Request};
use futures_util::{SinkExt, StreamExt};
use magi_bridge_client::{
    BridgeClientError, BridgeErrorLayer, ChatToolCall, HttpMcpServerConfig, McpBridgeClient,
    McpServerClient, McpServerConfig, McpServerConnectionConfig, McpToolCallRequest,
    ModelBridgeClient, ModelInvocationRequest, ModelResponse, ModelResponseStatus,
    ModelStreamingDelta, model_invocation_cancelled_error,
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
use magi_orchestrator::{OrchestratedExecutionRuntime, task_store::TaskStore};
use magi_session_store::{
    ActiveExecutionTurn, CanonicalTurn, CanonicalTurnItemKind, SessionStore, TimelineEntryInput,
    TimelineEntryKind,
};
use magi_tool_runtime::{ExternalMcpToolExecutor, ExternalToolCatalogSnapshot, ToolRegistry};
use magi_worker_runtime::WorkerRuntime;
use magi_workspace::WorkspaceStore;
use std::time::{Duration, Instant};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
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
    provider_first_raw_delta_at: Option<Instant>,
    provider_first_delta_at: Option<Instant>,
    first_stream_event_at: Option<Instant>,
    first_stream_event_sequence: Option<u64>,
    terminal_observed_at: Option<Instant>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HarnessTimingSnapshot {
    pub accepted_returned_ms: Option<u128>,
    pub provider_request_started_ms: Option<u128>,
    pub provider_first_raw_delta_ms: Option<u128>,
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

    fn mark_provider_first_raw_delta(&self) {
        let mut state = self.state.lock().expect("harness timing state should hold");
        if state.provider_first_raw_delta_at.is_none() {
            state.provider_first_raw_delta_at = Some(Instant::now());
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
            provider_first_raw_delta_ms: elapsed(state.provider_first_raw_delta_at),
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
    ToolSequenceThenCompleted {
        steps: Vec<(String, String)>,
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

    /// 前 `failures` 次调用以 `error` 失败，之后才返回正常回复。
    pub fn set_transient_failures_then_completed(
        &self,
        response: impl Into<String>,
        error: impl Into<String>,
        failures: usize,
    ) {
        let mut state = self
            .state
            .lock()
            .expect("harness provider state should hold");
        state.behavior = Some(ProviderBehavior::TransientThenCompleted {
            response: response.into(),
            error: error.into(),
        });
        state.transient_failures_remaining = failures;
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

    pub fn set_tool_sequence_then_completed(
        &self,
        steps: Vec<(impl Into<String>, impl Into<String>)>,
        response: impl Into<String>,
    ) {
        let mut state = self
            .state
            .lock()
            .expect("harness provider state should hold");
        state.behavior = Some(ProviderBehavior::ToolSequenceThenCompleted {
            steps: steps
                .into_iter()
                .map(|(tool, arguments)| (tool.into(), arguments.into()))
                .collect(),
            response: response.into(),
        });
        state.tool_round_emitted = 0;
        state.tool_round_limit = 0;
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
        if track_timing {
            if !delta.content.is_empty()
                || !delta.thinking.is_empty()
                || !delta.tool_calls.is_empty()
            {
                self.timing.mark_provider_first_raw_delta();
            }
            if !delta.content.is_empty() || !delta.thinking.is_empty() {
                self.timing.mark_provider_first_delta();
            }
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
                        tool_calls: Vec::new(),
                        ..Default::default()
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
                        tool_calls: Vec::new(),
                        ..Default::default()
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
                    let tool_call = ChatToolCall {
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
                    };
                    self.record_delta(
                        &ModelStreamingDelta {
                            content: String::new(),
                            thinking: String::new(),
                            tool_calls: vec![tool_call.clone()],
                            ..Default::default()
                        },
                        on_delta,
                        track_timing,
                    );
                    return Ok(ModelResponse {
                        status: ModelResponseStatus::RequiresToolExecution,
                        content: None,
                        thinking: None,
                        tool_calls: vec![tool_call],
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
                        let tool_call = ChatToolCall {
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
                        };
                        self.record_delta(
                            &ModelStreamingDelta {
                                content: String::new(),
                                thinking: String::new(),
                                tool_calls: vec![tool_call.clone()],
                                ..Default::default()
                            },
                            on_delta,
                            track_timing,
                        );
                        return Ok(ModelResponse {
                            status: ModelResponseStatus::RequiresToolExecution,
                            content: None,
                            thinking: None,
                            tool_calls: vec![tool_call],
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
                        tool_calls: Vec::new(),
                        ..Default::default()
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
                    let tool_call = ChatToolCall {
                        id: format!("harness-tool-call-{tool_round_index}"),
                        kind: "function".to_string(),
                        function: magi_bridge_client::ChatToolFunction {
                            name: tool_name,
                            arguments,
                        },
                    };
                    self.record_delta(
                        &ModelStreamingDelta {
                            content: String::new(),
                            thinking: String::new(),
                            tool_calls: vec![tool_call.clone()],
                            ..Default::default()
                        },
                        on_delta,
                        track_timing,
                    );
                    return Ok(ModelResponse {
                        status: ModelResponseStatus::RequiresToolExecution,
                        content: None,
                        thinking: None,
                        tool_calls: vec![tool_call],
                        usage: None,
                        finish_reason: Some("tool_calls".to_string()),
                        provider_context: Vec::new(),
                    });
                }
                let expected_tool_missing = request
                    .tools
                    .as_ref()
                    .is_none_or(|tools| !tools.iter().any(|tool| tool.function.name == tool_name));
                let tool_choice_conflict = request.tool_choice.as_ref().is_some_and(|choice| {
                    choice.kind == "function" && choice.function.name != tool_name
                });
                if !request.prompt.contains("Session Turn 编排分类器")
                    && (expected_tool_missing || tool_choice_conflict)
                {
                    return Err(BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Protocol,
                        code: Some(-32_004),
                        message: format!(
                            "harness provider contract mismatch: expected tool {tool_name} was not exposed or selected"
                        ),
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
                        tool_calls: Vec::new(),
                        ..Default::default()
                    };
                    self.record_delta(&delta, on_delta, track_timing);
                }
                Ok(ModelResponse::completed(response))
            }
            ProviderBehavior::ToolSequenceThenCompleted { steps, response } => {
                let next_tool = {
                    let mut state = self
                        .state
                        .lock()
                        .expect("harness provider state should hold");
                    let classifier_request = request.prompt.contains("Session Turn 编排分类器");
                    let next_tool = steps.get(state.tool_round_emitted).cloned();
                    let should_emit = !classifier_request
                        && state.tool_round_emitted < steps.len()
                        && next_tool.as_ref().is_some_and(|(tool_name, _)| {
                            request.tools.as_ref().is_some_and(|tools| {
                                tools.iter().any(|tool| tool.function.name == *tool_name)
                            })
                        });
                    if should_emit {
                        state.tool_round_emitted += 1;
                        next_tool
                    } else {
                        None
                    }
                };
                if let Some((tool_name, arguments)) = next_tool {
                    let tool_call = ChatToolCall {
                        id: format!(
                            "harness-tool-call-{}",
                            self.state
                                .lock()
                                .expect("harness provider state should hold")
                                .tool_round_emitted
                        ),
                        kind: "function".to_string(),
                        function: magi_bridge_client::ChatToolFunction {
                            name: tool_name,
                            arguments,
                        },
                    };
                    self.record_delta(
                        &ModelStreamingDelta {
                            content: String::new(),
                            thinking: String::new(),
                            tool_calls: vec![tool_call.clone()],
                            ..Default::default()
                        },
                        on_delta,
                        track_timing,
                    );
                    return Ok(ModelResponse {
                        status: ModelResponseStatus::RequiresToolExecution,
                        content: None,
                        thinking: None,
                        tool_calls: vec![tool_call],
                        usage: None,
                        finish_reason: Some("tool_calls".to_string()),
                        provider_context: Vec::new(),
                    });
                }
                let emitted_steps = self
                    .state
                    .lock()
                    .expect("harness provider state should hold")
                    .tool_round_emitted;
                let expected_step_missing =
                    steps.get(emitted_steps).is_some_and(|(tool_name, _)| {
                        request.tools.as_ref().is_none_or(|tools| {
                            !tools.iter().any(|tool| tool.function.name == *tool_name)
                        })
                    });
                if !request.prompt.contains("Session Turn 编排分类器") && expected_step_missing
                {
                    return Err(BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Protocol,
                        code: Some(-32_004),
                        message: "harness provider contract mismatch: expected tool sequence step was not exposed or selected"
                            .to_string(),
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
                        tool_calls: Vec::new(),
                        ..Default::default()
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
    external_mcp_catalog: Option<ExternalToolCatalogSnapshot>,
    external_mcp_executor: Option<ExternalMcpToolExecutor>,
}

impl MagiTurnHarness {
    pub fn new(response: impl Into<String>) -> Self {
        Self::from_parts(
            Arc::new(SessionStore::default()),
            HarnessModelClient::new(response),
            true,
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
        Self::from_parts_with_external_mcp(session_store, provider, with_task_runtime, None, None)
    }

    /// 装配带真实外部 MCP 执行器的 Task harness。
    ///
    /// catalog 仍由测试显式提供，执行器则可以连接真实 stdio/HTTP MCP server。
    /// 这样权限判定、Provider tool-call、Task/Turn 终态和外部副作用都经过同一
    /// TurnService 链路，不把 MCP 证据降级为 ToolRegistry 单测。
    pub fn new_task_with_external_mcp(
        response: impl Into<String>,
        catalog: ExternalToolCatalogSnapshot,
        executor: ExternalMcpToolExecutor,
    ) -> Self {
        Self::from_parts_with_external_mcp(
            Arc::new(SessionStore::default()),
            HarnessModelClient::new(response),
            true,
            Some(catalog),
            Some(executor),
        )
    }

    fn from_parts_with_external_mcp(
        session_store: Arc<SessionStore>,
        provider: HarnessModelClient,
        with_task_runtime: bool,
        external_mcp_catalog: Option<ExternalToolCatalogSnapshot>,
        external_mcp_executor: Option<ExternalMcpToolExecutor>,
    ) -> Self {
        let event_bus = Arc::new(InMemoryEventBus::new(512));
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let git_service = Arc::new(magi_git::GitService::new());
        let session_code_contexts = magi_git::SessionCodeContextRegistry::default();
        let workspace_git_coordinator = magi_git::WorkspaceGitOperationCoordinator::default();
        let snapshot_manager = Arc::new(magi_snapshot::SnapshotManager::new());
        let knowledge_store = Arc::new(magi_knowledge_store::KnowledgeStore::new());
        static HARNESS_RUNTIME_SEQUENCE: OnceLock<AtomicU64> = OnceLock::new();
        let runtime_sequence = HARNESS_RUNTIME_SEQUENCE
            .get_or_init(|| AtomicU64::new(0))
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let runtime_state_root = std::env::temp_dir().join(format!(
            "magi-turn-harness-runtime-{}-{}-{}",
            std::process::id(),
            UtcMillis::now().0,
            runtime_sequence
        ));
        let runtime_persistence = Arc::new(RuntimeStatePersistence::new(
            runtime_state_root.clone(),
            runtime_state_root.join("workspaces.json"),
            runtime_state_root.join("knowledge.json"),
        ));
        let git_tool_executor = crate::git_tool_runtime::build_git_tool_executor(
            crate::git_tool_runtime::GitToolRuntimeDependencies {
                git_service: git_service.clone(),
                session_code_contexts: session_code_contexts.clone(),
                workspace_git_coordinator: workspace_git_coordinator.clone(),
                event_bus: event_bus.clone(),
                knowledge_store: knowledge_store.clone(),
                snapshot_manager: snapshot_manager.clone(),
                runtime_persistence: runtime_persistence.clone(),
                managed_worktree_root: runtime_state_root.join("worktrees"),
            },
        );
        // 普通 Conversation harness 不创建 TaskStore；这与生产 profile 边界一致，
        // 不能只是不把已经创建的 TaskStore 挂到 ApiState 上。
        let task_store = with_task_runtime.then(|| Arc::new(TaskStore::new()));
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus))
            .with_git_tool_executor(git_tool_executor);
        if let Some(catalog) = external_mcp_catalog.clone() {
            tool_registry = tool_registry
                .with_external_tool_catalog_provider(Arc::new(move || catalog.clone()));
        }
        if let Some(executor) = external_mcp_executor.clone() {
            tool_registry = tool_registry.with_external_mcp_tool_executor(executor);
        }
        tool_registry.register_default_builtins();
        let mut execution_runtime = OrchestratedExecutionRuntime::new(WorkerRuntime::new());
        if let Some(task_store) = task_store.as_ref() {
            execution_runtime = execution_runtime.with_task_store(Arc::clone(task_store));
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
        .with_git_context_runtime(
            git_service.clone(),
            session_code_contexts.clone(),
            workspace_git_coordinator.clone(),
        )
        .with_snapshot_manager(snapshot_manager.clone())
        .with_knowledge_store(knowledge_store.clone())
        .with_runtime_persistence(runtime_persistence.clone())
        .with_tool_registry(tool_registry.clone());
        if let Some(task_store) = task_store.as_ref() {
            state = state.with_task_store(Arc::clone(task_store));
            state
                .task_execution_registry()
                .clone()
                .with_agent_spawn_preflight_runtime(AgentSpawnPreflightRuntime {
                    default_model_client: Some(Arc::new(provider.clone())),
                    session_code_contexts: Some(session_code_contexts.clone()),
                    git_service_configured: true,
                    ..AgentSpawnPreflightRuntime::default()
                });
        }
        let mut dispatcher_builder = LlmTaskDispatcher::new(
            Arc::clone(&event_bus),
            ExecutionPipeline {
                execution_runtime,
                memory_store: MemoryStore::new(),
            },
            LlmTaskDispatcherDependencies {
                session_store: Arc::clone(&session_store),
                execution_registry: state.task_execution_registry().clone(),
                result_receiver: Arc::clone(&result_receiver),
                conversation_registry: Arc::clone(&state.conversation_registry),
                agent_role_registry: Arc::clone(&state.agent_role_registry),
            },
            std::env::temp_dir().join(format!(
                "magi-turn-harness-{}-{}",
                std::process::id(),
                UtcMillis::now().0
            )),
        )
        .with_model_bridge_client(Arc::new(provider.clone()))
        .with_workspace_registry(Arc::clone(&state.workspace_registry))
        .with_git_context_runtime(git_service, session_code_contexts)
        .with_workspace_git_coordinator(workspace_git_coordinator)
        .with_snapshot_manager(snapshot_manager);
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
        let harness = Self {
            state,
            provider,
            external_mcp_catalog,
            external_mcp_executor,
        };
        harness.state.restore_turn_coordinator_from_session_store();
        harness
    }

    /// 使用同一份 canonical SessionStore 构造新的进程内状态，验证 daemon 重启后的
    /// request replay 和 Coordinator 身份恢复；EventBus、dispatcher 和 TaskStore 均重建。
    pub fn restart(&self) -> Self {
        Self::from_parts_with_external_mcp(
            Arc::clone(&self.state.session_store),
            self.provider.clone(),
            self.state.runner_manager().is_some(),
            self.external_mcp_catalog.clone(),
            self.external_mcp_executor.clone(),
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
                resume: false,
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
                command: None,
                workflow_id: None,
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
                resume: false,
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
                command: None,
                workflow_id: None,
            })
            .await;
        if response.is_ok() {
            self.provider.timing.mark_accepted_returned();
        }
        response
    }

    #[allow(clippy::too_many_arguments)]
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
                resume: false,
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
                command: None,
                workflow_id: None,
            })
            .await;
        if response.is_ok() {
            self.provider.timing.mark_accepted_returned();
        }
        response
    }

    /// 以目标模式提交工作区里的一轮（新目标的起始轮）。
    pub async fn submit_workspace_goal_start(
        &self,
        session_id: &SessionId,
        workspace_id: &magi_core::WorkspaceId,
        workspace_path: &Path,
        text: &str,
        request_id: &str,
    ) -> Result<SessionTurnResponseDto, crate::errors::ApiError> {
        self.provider.begin_timing();
        TurnService::new(self.state.clone())
            .submit(SessionTurnRequestDto {
                desktop_browser_tools_allowed: false,
                session_id: Some(session_id.to_string()),
                scope: SessionScopeKindDto::Workspace,
                workspace_id: Some(workspace_id.to_string()),
                workspace_path: Some(workspace_path.display().to_string()),
                text: Some(text.to_string()),
                skill_name: None,
                locale: Some("zh-CN".to_string()),
                goal_mode: true,
                resume: false,
                images: Vec::new(),
                context_references: Vec::new(),
                browser_annotation_refs: Vec::new(),
                browser_node_selections: Vec::new(),
                access_profile: Some(AccessProfile::FullAccess),
                orchestrator_session_config: None,
                request_id: Some(request_id.to_string()),
                user_message_id: None,
                placeholder_message_id: None,
                steer_current_turn: false,
                expected_turn_id: None,
                replace_turn_id: None,
                command: None,
                workflow_id: None,
            })
            .await
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
                resume: false,
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
                command: None,
                workflow_id: None,
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
                && turn.status.is_terminal()
            {
                self.provider.timing.mark_terminal_observed();
                return turn;
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
                    && event.payload.get("stream_delta").is_some()
            }) {
                if let Some(event) = events.iter().find(|event| {
                    event.event_type == "session.turn.item"
                        && event
                            .payload
                            .get("turn_id")
                            .and_then(serde_json::Value::as_str)
                            == Some(turn_id)
                        && event.payload.get("stream_delta").is_some()
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
                && matches!(
                    task.status,
                    magi_core::TaskStatus::Completed
                        | magi_core::TaskStatus::Failed
                        | magi_core::TaskStatus::Killed
                )
            {
                return task;
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
            .collect::<Vec<_>>()
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

    async fn submit_restricted_external_mcp_turn(
        harness: &MagiTurnHarness,
        session_id: &SessionId,
        workspace_id: &magi_core::WorkspaceId,
        workspace_path: &Path,
        target: &Path,
        request_id: &str,
    ) -> SessionTurnResponseDto {
        harness.provider.set_tool_sequence_then_completed(
            vec![
                (
                    "tool_catalog".to_string(),
                    serde_json::json!({
                        "include_external": true,
                        "include_mcp_servers": true,
                    })
                    .to_string(),
                ),
                (
                    "mcp__cancel_mcp__write_file".to_string(),
                    serde_json::json!({"path": target.display().to_string()}).to_string(),
                ),
            ],
            "MCP cancellation final response",
        );
        harness
            .submit_workspace_task_with_access_profile(
                session_id,
                workspace_id,
                workspace_path,
                "请以复杂任务模式执行：调用 MCP 写入文件并等待审批，授权后汇总结果",
                request_id,
                &format!("user-{request_id}"),
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Restricted MCP Turn should be accepted")
    }

    fn non_classifier_provider_request_count(harness: &MagiTurnHarness) -> usize {
        harness
            .provider
            .requests()
            .into_iter()
            .filter(|request| !request.prompt.contains("Session Turn 编排分类器"))
            .count()
    }

    fn failed_model_diagnostic_detail(turn: &CanonicalTurn) -> Option<&str> {
        turn.items
            .iter()
            .find(|item| {
                item.kind == CanonicalTurnItemKind::AssistantText
                    && item.status == magi_session_store::CanonicalTurnItemStatus::Failed
            })
            .and_then(|item| item.metadata.get("modelFailure"))
            .and_then(|failure| failure.get("detail"))
            .and_then(serde_json::Value::as_str)
    }

    async fn run_read_only_explicit_file_tool_case<F>(
        tool_name: &'static str,
        request_suffix: &'static str,
        prompt: String,
        configure: F,
    ) -> (tempfile::TempDir, PathBuf, CanonicalTurn, magi_core::Task)
    where
        F: FnOnce(&Path) -> (PathBuf, serde_json::Value),
    {
        let harness = MagiTurnHarness::new_task(format!("只读模式拒绝显式 {tool_name}"));
        let workspace_root =
            tempfile::tempdir().expect("read-only explicit file tool workspace should create");
        let (target, arguments) = configure(workspace_root.path());
        let workspace_id =
            magi_core::WorkspaceId::new(format!("harness-read-only-{request_suffix}-workspace"));
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("read-only explicit file tool workspace should register");
        let session_id = SessionId::new(format!("harness-read-only-{request_suffix}-session"));
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                format!("只读显式 {tool_name} 验收"),
                Some(workspace_id.to_string()),
            )
            .expect("read-only explicit file tool session should create");
        harness.provider.set_tool_then_completed(
            tool_name,
            arguments.to_string(),
            "只读模式不会执行文件写工具",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                &prompt,
                &format!("harness-read-only-{request_suffix}-request"),
                &format!("harness-read-only-{request_suffix}-user"),
                Some(AccessProfile::ReadOnly),
            )
            .await
            .expect("read-only explicit file tool task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("read-only explicit file tool should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("read-only explicit file tool should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Failed, "tool={tool_name}");
        assert_eq!(
            task.status,
            magi_core::TaskStatus::Failed,
            "tool={tool_name}"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            1,
            "ReadOnly 隐藏显式 {tool_name} 仍需一次 Provider 请求以收口工具契约错误"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "ReadOnly 隐藏显式 {tool_name} 后不得发布审批请求"
        );
        assert!(
            failed_model_diagnostic_detail(&turn).is_some_and(
                |detail| detail.contains(tool_name) && detail.contains("was not exposed")
            ),
            "ReadOnly 显式隐藏的写工具应通过 modelFailure 诊断说明 Provider 工具契约错误；items={:#?}",
            turn.items
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": format!("read_only_{request_suffix}"),
                "tool": tool_name,
                "surface": if tool_name.starts_with("git_") {
                    "git_workspace"
                } else if tool_name == "image_generate" {
                    "image_generation"
                } else {
                    "file_executor"
                },
                "access_profile": "ReadOnly",
                "scope": "workspace_internal",
                "lifecycle": "deny",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "failed",
                "task_status": "failed",
                "side_effect": "write_blocked",
            }),
            &turn,
        );
        (workspace_root, target, turn, task)
    }

    async fn run_restricted_outside_file_tool_case<F>(
        tool_name: &'static str,
        request_suffix: &'static str,
        prompt: impl Into<String>,
        configure: F,
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        PathBuf,
        CanonicalTurn,
        magi_core::Task,
    )
    where
        F: FnOnce(&Path, &Path) -> (PathBuf, serde_json::Value),
    {
        let prompt = prompt.into();
        let harness = MagiTurnHarness::new_task(format!("受限模式拒绝工作区外 {tool_name}"));
        let workspace_root =
            tempfile::tempdir().expect("restricted outside file tool workspace should create");
        let outside_root =
            tempfile::tempdir().expect("restricted outside file tool target should create");
        let (observed_target, arguments) = configure(workspace_root.path(), outside_root.path());
        let workspace_id = magi_core::WorkspaceId::new(format!(
            "harness-restricted-outside-{request_suffix}-workspace"
        ));
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("restricted outside file tool workspace should register");
        let session_id = SessionId::new(format!(
            "harness-restricted-outside-{request_suffix}-session"
        ));
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                format!("受限模式工作区外 {tool_name} 验收"),
                Some(workspace_id.to_string()),
            )
            .expect("restricted outside file tool session should create");
        harness.provider.set_tool_then_completed(
            tool_name,
            arguments.to_string(),
            "工作区外文件操作不应执行",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                &prompt,
                &format!("harness-restricted-outside-{request_suffix}-request"),
                &format!("harness-restricted-outside-{request_suffix}-user"),
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("restricted outside file tool task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("restricted outside file tool should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("restricted outside file tool should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "工作区外 {tool_name} 被确定性拒绝后不得重试 Provider"
        );
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty(),
            "工作区外 {tool_name} 不得创建 pending approval"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "工作区外 {tool_name} 不得发布审批请求"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item
                    .tool
                    .as_ref()
                    .is_some_and(|tool| tool.name == tool_name && tool.error.is_some())
        }));
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": format!("restricted_external_{request_suffix}"),
                "tool": tool_name,
                "surface": "file_executor",
                "access_profile": "Restricted",
                "scope": "workspace_external",
                "lifecycle": "deny",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "failed",
                "task_status": "failed",
                "side_effect": "external_path_blocked",
            }),
            &turn,
        );
        (workspace_root, outside_root, observed_target, turn, task)
    }

    async fn run_full_access_file_tool_case<F>(
        tool_name: &'static str,
        request_suffix: &'static str,
        prompt: &'static str,
        configure: F,
    ) -> (tempfile::TempDir, PathBuf, CanonicalTurn, magi_core::Task)
    where
        F: FnOnce(&Path) -> (PathBuf, serde_json::Value),
    {
        let harness = MagiTurnHarness::new_task(format!("完全授权 {tool_name}"));
        let workspace_root =
            tempfile::tempdir().expect("full access file tool workspace should create");
        let (observed_target, arguments) = configure(workspace_root.path());
        let workspace_id =
            magi_core::WorkspaceId::new(format!("harness-full-access-{request_suffix}-workspace"));
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("full access file tool workspace should register");
        let session_id = SessionId::new(format!("harness-full-access-{request_suffix}-session"));
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                format!("完全授权 {tool_name} 验收"),
                Some(workspace_id.to_string()),
            )
            .expect("full access file tool session should create");
        harness.provider.set_tool_then_completed(
            tool_name,
            arguments.to_string(),
            "完全授权文件工具完成",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                prompt,
                &format!("harness-full-access-{request_suffix}-request"),
                &format!("harness-full-access-{request_suffix}-user"),
                Some(AccessProfile::FullAccess),
            )
            .await
            .expect("full access file tool task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("full access file tool should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("full access file tool should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "完全授权 {tool_name} 应执行工具轮和一次最终答复轮"
        );
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty(),
            "完全授权 {tool_name} 不应创建 pending approval"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "完全授权 {tool_name} 不应发布审批请求"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == tool_name && tool.result.is_some() && tool.error.is_none()
                })
        }));
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": format!("full_access_{request_suffix}"),
                "tool": tool_name,
                "surface": "file_executor",
                "access_profile": "FullAccess",
                "scope": "workspace_internal",
                "lifecycle": "allow",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "file_operation_executed",
            }),
            &turn,
        );
        (workspace_root, observed_target, turn, task)
    }

    async fn run_full_access_external_file_tool_case<F>(
        tool_name: &'static str,
        request_suffix: &'static str,
        prompt: &'static str,
        side_effect: &'static str,
        configure: F,
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        PathBuf,
        CanonicalTurn,
        magi_core::Task,
    )
    where
        F: FnOnce(&Path, &Path) -> (PathBuf, serde_json::Value),
    {
        let harness = MagiTurnHarness::new_task(format!("完全授权工作区外 {tool_name}"));
        let workspace_root =
            tempfile::tempdir().expect("full access external file tool workspace should create");
        let outside_root =
            tempfile::tempdir().expect("full access external file tool target should create");
        let (observed_target, arguments) = configure(workspace_root.path(), outside_root.path());
        let workspace_id = magi_core::WorkspaceId::new(format!(
            "harness-full-access-external-{request_suffix}-workspace"
        ));
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("full access external file tool workspace should register");
        let session_id = SessionId::new(format!(
            "harness-full-access-external-{request_suffix}-session"
        ));
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                format!("完全授权工作区外 {tool_name} 验收"),
                Some(workspace_id.to_string()),
            )
            .expect("full access external file tool session should create");
        harness.provider.set_tool_then_completed(
            tool_name,
            arguments.to_string(),
            "完全授权工作区外文件工具完成",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                prompt,
                &format!("harness-full-access-external-{request_suffix}-request"),
                &format!("harness-full-access-external-{request_suffix}-user"),
                Some(AccessProfile::FullAccess),
            )
            .await
            .expect("full access external file tool task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("full access external file tool should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("full access external file tool should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "完全授权工作区外 {tool_name} 应执行工具轮和一次最终答复轮"
        );
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty(),
            "完全授权工作区外 {tool_name} 不应创建 pending approval"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "完全授权工作区外 {tool_name} 不应发布审批请求"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == tool_name && tool.result.is_some() && tool.error.is_none()
                })
        }));
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": format!("full_access_external_{request_suffix}"),
                "tool": tool_name,
                "surface": "file_executor",
                "access_profile": "FullAccess",
                "scope": "workspace_external",
                "lifecycle": "allow",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": side_effect,
            }),
            &turn,
        );
        (workspace_root, outside_root, observed_target, turn, task)
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
        let first_raw_delta = timings
            .iter()
            .map(|timing| timing_metric(timing, |timing| timing.provider_first_raw_delta_ms))
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
            timing.accepted_returned_ms <= timing.provider_first_raw_delta_ms
                && timing.provider_first_raw_delta_ms <= timing.provider_first_delta_ms
                && timing.provider_first_delta_ms <= timing.first_stream_event_ms
                && timing.first_stream_event_ms <= timing.terminal_observed_ms
                && timing
                    .first_stream_event_sequence
                    .is_some_and(|sequence| sequence > 0)
        }));
        eprintln!(
            "LOCAL_MOCK_P95 scenario={name} samples={} accepted_ms={{p50:{},p95:{},max:{}}} first_raw_delta_ms={{p50:{},p95:{},max:{}}} first_delta_ms={{p50:{},p95:{},max:{}}} first_event_ms={{p50:{},p95:{},max:{}}} terminal_ms={{p50:{},p95:{},max:{}}}",
            timings.len(),
            percentile(&accepted, 50),
            percentile(&accepted, 95),
            accepted.iter().copied().max().unwrap_or_default(),
            percentile(&first_raw_delta, 50),
            percentile(&first_raw_delta, 95),
            first_raw_delta.iter().copied().max().unwrap_or_default(),
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
    async fn task_profile_restart_replays_completed_turn_without_provider_reexecution() {
        let harness = MagiTurnHarness::new_task("任务重启回放");
        let request_id = "harness-task-restart-replay-request";
        let user_message_id = "harness-task-restart-replay-user";
        let text = "请执行一个任务并在重启后验证终态回放";
        let first = harness
            .submit_task(text, request_id, user_message_id)
            .await
            .expect("首次任务提交应成功");
        let session_id = SessionId::new(first.session_id.clone());
        let turn_id = first.turn_id.clone().expect("任务提交应有 Turn");
        let root_task_id = first.root_task_id.clone().expect("任务提交应有 root task");
        let completed = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(completed.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);

        let request_count = harness.provider.requests().len();
        let restarted = harness.restart();
        let replay = restarted
            .submit_task(text, request_id, user_message_id)
            .await
            .expect("重启后的任务请求应返回 replay");

        assert_eq!(replay.turn_id.as_deref(), Some(turn_id.as_str()));
        assert_eq!(restarted.provider.requests().len(), request_count);
        assert_eq!(
            restarted
                .state
                .session_store
                .canonical_turn_for_session_turn_id(&session_id, &turn_id)
                .expect("重启后应保留 canonical Turn")
                .status,
            CanonicalTurnStatus::Completed
        );
    }

    #[tokio::test]
    async fn restricted_profile_allow_for_turn_requires_new_approval_on_next_turn() {
        let harness = MagiTurnHarness::new_task("跨 Turn 授权隔离");
        let workspace_root = tempfile::tempdir().expect("cross turn workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-cross-turn-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("cross turn workspace should register");
        let session_id = SessionId::new("harness-cross-turn-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "跨 Turn 授权会话",
                Some(workspace_id.to_string()),
            )
            .expect("cross turn session should create");

        let first_target = workspace_root.path().join("cross-turn-first.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf first > {}", first_target.display())
            })
            .to_string(),
            "第一轮完成",
        );
        let first = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "第一轮执行 shell_exec 并在 Turn 内复用授权",
                "harness-cross-turn-request-1",
                "harness-cross-turn-user-1",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("第一轮提交应成功");
        let first_turn_id = first.turn_id.clone().expect("第一轮应有 Turn");
        let first_task_id = first.root_task_id.clone().expect("第一轮应有 root task");
        let first_pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            workspace_root.path(),
            &first_pending.approval_id,
            "allow_for_turn",
        )
        .await;
        assert_eq!(
            harness
                .wait_for_terminal(&session_id, &first_turn_id)
                .await
                .status,
            CanonicalTurnStatus::Completed
        );
        assert_eq!(
            harness
                .wait_for_task_terminal(&magi_core::TaskId::new(first_task_id))
                .await
                .status,
            magi_core::TaskStatus::Completed
        );

        let second_target = workspace_root.path().join("cross-turn-second.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf second > {}", second_target.display())
            })
            .to_string(),
            "第二轮完成",
        );
        let second = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "第二轮再次执行 shell_exec，必须重新审批",
                "harness-cross-turn-request-2",
                "harness-cross-turn-user-2",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("第二轮提交应成功");
        let second_turn_id = second.turn_id.clone().expect("第二轮应有 Turn");
        let second_task_id = second.root_task_id.clone().expect("第二轮应有 root task");
        let second_pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_ne!(
            first_pending.approval_id, second_pending.approval_id,
            "allow_for_turn 授权不得跨 Turn 复用"
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
        assert_eq!(
            harness
                .wait_for_terminal(&session_id, &second_turn_id)
                .await
                .status,
            CanonicalTurnStatus::Completed
        );
        assert_eq!(
            harness
                .wait_for_task_terminal(&magi_core::TaskId::new(second_task_id))
                .await
                .status,
            magi_core::TaskStatus::Completed
        );
        assert_eq!(
            fs::read_to_string(&first_target).expect("第一轮写入应存在"),
            "first"
        );
        assert_eq!(
            fs::read_to_string(&second_target).expect("第二轮写入应存在"),
            "second"
        );

        let other_session_id = SessionId::new("harness-cross-session-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                other_session_id.clone(),
                "跨 Session 授权会话",
                Some(workspace_id.to_string()),
            )
            .expect("cross session should create");
        let third_target = workspace_root.path().join("cross-session.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf third > {}", third_target.display())
            })
            .to_string(),
            "跨 Session 完成",
        );
        let third = harness
            .submit_workspace_task_with_access_profile(
                &other_session_id,
                &workspace_id,
                workspace_root.path(),
                "新 Session 再次执行 shell_exec，必须独立审批",
                "harness-cross-session-request",
                "harness-cross-session-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("跨 Session 提交应成功");
        let third_turn_id = third.turn_id.clone().expect("跨 Session 应有 Turn");
        let third_task_id = third
            .root_task_id
            .clone()
            .expect("跨 Session 应有 root task");
        let third_pending = wait_for_pending_tool_approval(&harness, &other_session_id).await;
        assert_ne!(
            second_pending.approval_id, third_pending.approval_id,
            "allow_for_turn 授权不得跨 Session 复用"
        );
        resolve_tool_approval_via_http(
            &harness,
            &other_session_id,
            &workspace_id,
            workspace_root.path(),
            &third_pending.approval_id,
            "allow_once",
        )
        .await;
        assert_eq!(
            harness
                .wait_for_terminal(&other_session_id, &third_turn_id)
                .await
                .status,
            CanonicalTurnStatus::Completed
        );
        assert_eq!(
            harness
                .wait_for_task_terminal(&magi_core::TaskId::new(third_task_id))
                .await
                .status,
            magi_core::TaskStatus::Completed
        );
        assert_eq!(
            fs::read_to_string(&third_target).expect("跨 Session 写入应存在"),
            "third"
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .into_iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            2,
            "两个 Turn 必须分别产生审批请求"
        );
        assert_eq!(
            harness
                .events_for(&other_session_id)
                .into_iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1,
            "新 Session 必须产生独立审批请求"
        );
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
            crate::dto::SessionTurnRouteDto::Execute
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
        assert!(
            harness.provider.deltas().iter().any(|delta| {
                !delta.tool_calls.is_empty()
                    && delta.content.is_empty()
                    && delta.thinking.is_empty()
            }),
            "工具调用轮必须保留没有可见正文的 raw tool-call delta"
        );
        let timing = harness.provider.timing();
        assert!(
            timing.provider_first_raw_delta_ms.is_some(),
            "工具调用轮必须记录 raw tool-call 首 delta"
        );
        assert!(timing.provider_first_delta_ms.is_some());
        assert!(
            timing.provider_first_raw_delta_ms <= timing.provider_first_delta_ms,
            "raw tool-call 首 delta 应先于可见正文首 delta"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn task_profile_real_stdio_mcp_call_is_bound_to_turn_and_terminal() {
        let workspace_root = tempfile::tempdir().expect("MCP Turn workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-mcp-turn-workspace");
        let session_id = SessionId::new("harness-mcp-turn-session");
        let target = workspace_root.path().join("mcp-turn-side-effect.txt");
        let script = format!(
            r#"
while IFS= read -r line; do
    method=$(echo "$line" | grep -o '"method":"[^"]*"' | head -1 | cut -d'"' -f4)
    id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
    case "$method" in
        initialize)
            printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":"2024-11-05","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"turn-mcp","version":"1.0"}}}}}}\n' "$id"
            ;;
        notifications/initialized)
            ;;
        tools/call)
            printf 'mcp-stdio-turn-side-effect\n' > '{target}'
            printf '{{"jsonrpc":"2.0","id":%s,"result":{{"content":[{{"type":"text","text":"stdio-turn-ok"}}]}}}}\n' "$id"
            ;;
    esac
done
"#,
            target = target.display(),
        );
        let client = Arc::new(McpServerClient::from_stdio(McpServerConfig::new(
            "sh",
            vec!["-c".to_string(), script],
        )));
        let catalog = ExternalToolCatalogSnapshot {
            mcp_tools: vec![magi_tool_runtime::ExternalMcpToolCatalogEntry {
                server_id: "turn-mcp".to_string(),
                server_name: "Turn MCP".to_string(),
                model_tool_name: "mcp__turn_mcp__write_file".to_string(),
                tool_name: "write_file".to_string(),
                description: "Write one MCP fixture file".to_string(),
                read_only: false,
                input_schema: serde_json::json!({"type":"object"}),
            }],
            ..ExternalToolCatalogSnapshot::default()
        };
        let executor: ExternalMcpToolExecutor = Arc::new(move |server, tool, arguments| {
            let response = client
                .call_tool(McpToolCallRequest {
                    server_name: server.to_string(),
                    tool_name: tool.to_string(),
                    input: arguments.to_string(),
                })
                .expect("real stdio MCP call should succeed through TurnService");
            (
                response.payload,
                if response.ok {
                    magi_core::ExecutionResultStatus::Succeeded
                } else {
                    magi_core::ExecutionResultStatus::Failed
                },
            )
        });
        let harness =
            MagiTurnHarness::new_task_with_external_mcp("MCP Turn 最终答复", catalog, executor);
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("MCP Turn workspace should register");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "MCP Turn 真实链路",
                Some(workspace_id.to_string()),
            )
            .expect("MCP Turn session should create");
        harness.provider.set_tool_sequence_then_completed(
            vec![
                (
                    "tool_catalog".to_string(),
                    serde_json::json!({
                        "include_external": true,
                        "include_mcp_servers": true,
                    })
                    .to_string(),
                ),
                (
                    "mcp__turn_mcp__write_file".to_string(),
                    serde_json::json!({"path": target}).to_string(),
                ),
            ],
            "MCP Turn 最终答复",
        );

        let request_id = "harness-mcp-turn-request";
        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "执行一个任务：调用真实 stdio MCP 工具写入 fixture，然后返回结果",
                request_id,
                "harness-mcp-turn-user",
                Some(AccessProfile::FullAccess),
            )
            .await
            .expect("真实 MCP Task Turn 应被接纳");
        let turn_id = response.turn_id.clone().expect("MCP Turn 应有 turn_id");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("MCP Task 应有 root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;

        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(&target).expect("真实 MCP server 应写入 fixture"),
            "mcp-stdio-turn-side-effect\n"
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 3);
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item
                    .tool
                    .as_ref()
                    .is_some_and(|tool| tool.name == "mcp__turn_mcp__write_file")
        }));
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item.content.as_deref() == Some("MCP Turn 最终答复")
        }));
        record_unified_permission_matrix_row(serde_json::json!({
            "case": "mcp_stdio_real_turn_full_access",
            "tool": "mcp__turn_mcp__write_file",
            "surface": "mcp_executor",
            "access_profile": "FullAccess",
            "scope": "external",
            "lifecycle": "allow",
            "approval_requested": false,
            "approval_resolved": false,
            "provider_requests": non_classifier_provider_request_count(&harness),
            "turn_status": "completed",
            "task_status": "completed",
            "side_effect": "stdio_server_wrote_file",
            "request_id": request_id,
            "turn_id": turn_id,
            "execution_profile": "task",
        }));
    }

    #[tokio::test]
    async fn read_only_task_turn_allows_real_stdio_mcp_read_and_records_identity() {
        let workspace_root = tempfile::tempdir().expect("ReadOnly MCP workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-read-only-mcp-workspace");
        let session_id = SessionId::new("harness-read-only-mcp-session");
        let target = workspace_root.path().join("read-only-mcp-source.txt");
        fs::write(&target, "read-only-mcp-result")
            .expect("ReadOnly MCP source fixture should write");
        let script = format!(
            r#"
while IFS= read -r line; do
    method=$(echo "$line" | grep -o '"method":"[^"]*"' | head -1 | cut -d'"' -f4)
    id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
    case "$method" in
        initialize)
            printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":"2024-11-05","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"read-only-mcp","version":"1.0"}}}}}}\n' "$id"
            ;;
        notifications/initialized)
            ;;
        tools/call)
            content=$(cat '{target}')
            printf '{{"jsonrpc":"2.0","id":%s,"result":{{"content":[{{"type":"text","text":"%s"}}]}}}}\n' "$id" "$content"
            ;;
    esac
done
"#,
            target = target.display(),
        );
        let client = Arc::new(McpServerClient::from_stdio(McpServerConfig::new(
            "sh",
            vec!["-c".to_string(), script],
        )));
        let executor_calls = Arc::new(AtomicU64::new(0));
        let executor_calls_for_executor = Arc::clone(&executor_calls);
        let catalog = ExternalToolCatalogSnapshot {
            mcp_tools: vec![magi_tool_runtime::ExternalMcpToolCatalogEntry {
                server_id: "read-only-mcp".to_string(),
                server_name: "Read Only MCP".to_string(),
                model_tool_name: "mcp__read_only_mcp__read_file".to_string(),
                tool_name: "read_file".to_string(),
                description: "Read one registered MCP fixture file".to_string(),
                read_only: true,
                input_schema: serde_json::json!({"type":"object"}),
            }],
            ..ExternalToolCatalogSnapshot::default()
        };
        let executor: ExternalMcpToolExecutor = Arc::new(move |server, tool, arguments| {
            let response = client
                .call_tool(McpToolCallRequest {
                    server_name: server.to_string(),
                    tool_name: tool.to_string(),
                    input: arguments.to_string(),
                })
                .expect("real stdio ReadOnly MCP call should succeed through TurnService");
            executor_calls_for_executor.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (
                response.payload,
                if response.ok {
                    magi_core::ExecutionResultStatus::Succeeded
                } else {
                    magi_core::ExecutionResultStatus::Failed
                },
            )
        });
        let harness =
            MagiTurnHarness::new_task_with_external_mcp("ReadOnly MCP 最终答复", catalog, executor);
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("ReadOnly MCP workspace should register");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "ReadOnly MCP 真实 Turn",
                Some(workspace_id.to_string()),
            )
            .expect("ReadOnly MCP session should create");
        harness.provider.set_tool_sequence_then_completed(
            vec![
                (
                    "tool_catalog".to_string(),
                    serde_json::json!({
                        "include_external": true,
                        "include_mcp_servers": true,
                    })
                    .to_string(),
                ),
                (
                    "mcp__read_only_mcp__read_file".to_string(),
                    serde_json::json!({"path": target}).to_string(),
                ),
            ],
            "ReadOnly MCP 最终答复",
        );

        let request_id = "harness-read-only-mcp-request";
        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "执行一个任务：通过注册为只读的 MCP 工具读取 fixture 并汇总结果",
                request_id,
                "harness-read-only-mcp-user",
                Some(AccessProfile::ReadOnly),
            )
            .await
            .expect("ReadOnly MCP Task Turn should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("ReadOnly MCP Turn should have identity");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("ReadOnly MCP Task should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;

        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(non_classifier_provider_request_count(&harness), 3);
        assert_eq!(executor_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "mcp__read_only_mcp__read_file"
                        && tool.result.is_some()
                        && tool.error.is_none()
                        && tool.result.as_ref().is_some_and(|result| {
                            result.to_string().contains("read-only-mcp-result")
                        })
                })
        }));
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "mcp_read_only_real_stdio_turn_read",
                "tool": "mcp__read_only_mcp__read_file",
                "surface": "mcp_executor",
                "access_profile": "ReadOnly",
                "scope": "external",
                "lifecycle": "allow",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "read_only_file_read",
            }),
            &turn,
        );
    }

    #[tokio::test]
    async fn task_profile_real_http_mcp_call_is_bound_to_turn_and_terminal() {
        let workspace_root = tempfile::tempdir().expect("HTTP MCP Turn workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-http-mcp-turn-workspace");
        let session_id = SessionId::new("harness-http-mcp-turn-session");
        let target = workspace_root.path().join("http-mcp-turn-side-effect.txt");
        let server_target = target.clone();
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("HTTP MCP Turn listener should bind");
        let address = listener
            .local_addr()
            .expect("HTTP MCP Turn listener should have address");
        let server = std::thread::spawn(move || {
            for request_index in 0..3 {
                let (mut stream, _) = listener
                    .accept()
                    .expect("HTTP MCP Turn server should accept request");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("HTTP MCP Turn read timeout should configure");
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 1024];
                let header_end = loop {
                    let read = stream
                        .read(&mut buffer)
                        .expect("HTTP MCP Turn request should be readable");
                    assert!(read > 0, "HTTP MCP Turn request closed before headers");
                    bytes.extend_from_slice(&buffer[..read]);
                    if let Some(position) =
                        bytes.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        break position + 4;
                    }
                };
                let header_text = String::from_utf8(bytes[..header_end].to_vec())
                    .expect("HTTP MCP Turn headers should be UTF-8");
                let content_length = header_text
                    .lines()
                    .find_map(|line| {
                        line.split_once(':').and_then(|(name, value)| {
                            (name.eq_ignore_ascii_case("content-length"))
                                .then(|| value.trim().parse::<usize>().expect("content length"))
                        })
                    })
                    .expect("HTTP MCP Turn request should include content length");
                while bytes.len() - header_end < content_length {
                    let read = stream
                        .read(&mut buffer)
                        .expect("HTTP MCP Turn body should be readable");
                    assert!(read > 0, "HTTP MCP Turn request closed before body");
                    bytes.extend_from_slice(&buffer[..read]);
                }
                let body: serde_json::Value =
                    serde_json::from_slice(&bytes[header_end..header_end + content_length])
                        .expect("HTTP MCP Turn body should be JSON");
                let method = body["method"]
                    .as_str()
                    .expect("HTTP MCP Turn request should contain method");
                let (status, headers, response_body) = match (request_index, method) {
                    (0, "initialize") => (
                        "200 OK",
                        "Content-Type: application/json\r\nMcp-Session-Id: turn-http-session\r\n",
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": body["id"],
                            "result": {
                                "protocolVersion": "2024-11-05",
                                "capabilities": {"tools": {}},
                                "serverInfo": {"name": "turn-http-mcp", "version": "1.0"}
                            }
                        })
                        .to_string(),
                    ),
                    (1, "notifications/initialized") => ("202 Accepted", "", String::new()),
                    (2, "tools/call") => {
                        fs::write(&server_target, "mcp-http-turn-side-effect\n")
                            .expect("HTTP MCP Turn server should write target");
                        (
                            "200 OK",
                            "Content-Type: application/json\r\n",
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": body["id"],
                                "result": {
                                    "content": [{"type": "text", "text": "http-turn-ok"}]
                                }
                            })
                            .to_string(),
                        )
                    }
                    other => panic!("unexpected HTTP MCP Turn request: {other:?}"),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{response_body}",
                    response_body.len(),
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("HTTP MCP Turn response should write");
            }
        });

        let client = Arc::new(McpServerClient::new(
            McpServerConnectionConfig::StreamableHttp(HttpMcpServerConfig {
                url: format!("http://{address}/mcp"),
                headers: std::collections::BTreeMap::new(),
                request_timeout: Duration::from_secs(2),
            }),
        ));
        let catalog = ExternalToolCatalogSnapshot {
            mcp_tools: vec![magi_tool_runtime::ExternalMcpToolCatalogEntry {
                server_id: "turn-http-mcp".to_string(),
                server_name: "Turn HTTP MCP".to_string(),
                model_tool_name: "mcp__turn_http_mcp__write_file".to_string(),
                tool_name: "write_file".to_string(),
                description: "Write one HTTP MCP fixture file".to_string(),
                read_only: false,
                input_schema: serde_json::json!({"type":"object"}),
            }],
            ..ExternalToolCatalogSnapshot::default()
        };
        let executor: ExternalMcpToolExecutor = Arc::new(move |server, tool, arguments| {
            let response = client
                .call_tool(McpToolCallRequest {
                    server_name: server.to_string(),
                    tool_name: tool.to_string(),
                    input: arguments.to_string(),
                })
                .expect("real HTTP MCP call should succeed through TurnService");
            (
                response.payload,
                if response.ok {
                    magi_core::ExecutionResultStatus::Succeeded
                } else {
                    magi_core::ExecutionResultStatus::Failed
                },
            )
        });
        let harness = MagiTurnHarness::new_task_with_external_mcp(
            "HTTP MCP Turn 最终答复",
            catalog,
            executor,
        );
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("HTTP MCP Turn workspace should register");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "HTTP MCP Turn 真实链路",
                Some(workspace_id.to_string()),
            )
            .expect("HTTP MCP Turn session should create");
        harness.provider.set_tool_sequence_then_completed(
            vec![
                (
                    "tool_catalog".to_string(),
                    serde_json::json!({
                        "include_external": true,
                        "include_mcp_servers": true,
                    })
                    .to_string(),
                ),
                (
                    "mcp__turn_http_mcp__write_file".to_string(),
                    serde_json::json!({"path": target}).to_string(),
                ),
            ],
            "HTTP MCP Turn 最终答复",
        );

        let request_id = "harness-http-mcp-turn-request";
        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "执行一个任务：调用真实 HTTP MCP 工具写入 fixture，然后返回结果",
                request_id,
                "harness-http-mcp-turn-user",
                Some(AccessProfile::FullAccess),
            )
            .await
            .expect("真实 HTTP MCP Task Turn 应被接纳");
        let turn_id = response
            .turn_id
            .clone()
            .expect("HTTP MCP Turn 应有 turn_id");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("HTTP MCP Task 应有 root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        server.join().expect("HTTP MCP Turn server should stop");

        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(&target).expect("真实 HTTP MCP server 应写入 fixture"),
            "mcp-http-turn-side-effect\n"
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 3);
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "mcp__turn_http_mcp__write_file"
                        && tool.result.is_some()
                        && tool.error.is_none()
                })
        }));
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item.content.as_deref() == Some("HTTP MCP Turn 最终答复")
        }));
        record_unified_permission_matrix_row(serde_json::json!({
            "case": "mcp_http_real_turn_full_access",
            "tool": "mcp__turn_http_mcp__write_file",
            "surface": "mcp_executor",
            "access_profile": "FullAccess",
            "scope": "external",
            "lifecycle": "allow",
            "approval_requested": false,
            "approval_resolved": false,
            "provider_requests": non_classifier_provider_request_count(&harness),
            "turn_status": "completed",
            "task_status": "completed",
            "side_effect": "http_server_wrote_file",
            "request_id": request_id,
            "turn_id": turn_id,
            "execution_profile": "task",
        }));
    }

    #[tokio::test]
    async fn task_profile_mcp_restricted_cancellation_isolated_across_turn_and_session() {
        let workspace_root = tempfile::tempdir().expect("MCP cancellation workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-mcp-cancel-workspace");
        let session_id = SessionId::new("harness-mcp-cancel-session");
        let other_session_id = SessionId::new("harness-mcp-cancel-other-session");
        let first_target = workspace_root.path().join("mcp-first.txt");
        let second_target = workspace_root.path().join("mcp-second.txt");
        let third_target = workspace_root.path().join("mcp-third.txt");
        let executor_calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let executor_calls_for_executor = Arc::clone(&executor_calls);
        let catalog = ExternalToolCatalogSnapshot {
            mcp_tools: vec![magi_tool_runtime::ExternalMcpToolCatalogEntry {
                server_id: "cancel-mcp".to_string(),
                server_name: "Cancellation MCP".to_string(),
                model_tool_name: "mcp__cancel_mcp__write_file".to_string(),
                tool_name: "write_file".to_string(),
                description: "Write one cancellation fixture file".to_string(),
                read_only: false,
                input_schema: serde_json::json!({"type":"object"}),
            }],
            ..ExternalToolCatalogSnapshot::default()
        };
        let executor: ExternalMcpToolExecutor = Arc::new(move |server, tool, arguments| {
            let input: serde_json::Value =
                serde_json::from_str(arguments).expect("MCP cancellation arguments should be JSON");
            let path = input["path"]
                .as_str()
                .expect("MCP cancellation arguments should contain path");
            fs::write(path, "mcp-cancellation-side-effect\n")
                .expect("MCP cancellation executor should write target");
            executor_calls_for_executor
                .lock()
                .expect("MCP cancellation executor audit lock should hold")
                .push(format!("{server}:{tool}:{path}"));
            (
                serde_json::json!({"status":"succeeded","path":path}).to_string(),
                magi_core::ExecutionResultStatus::Succeeded,
            )
        });
        let harness = MagiTurnHarness::new_task_with_external_mcp(
            "MCP cancellation final response",
            catalog,
            executor,
        );
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("MCP cancellation workspace should register");
        for current_session_id in [&session_id, &other_session_id] {
            harness
                .state
                .session_store
                .create_session_for_workspace(
                    (*current_session_id).clone(),
                    "MCP cancellation isolation",
                    Some(workspace_id.to_string()),
                )
                .expect("MCP cancellation session should create");
        }

        let first_requests_before = non_classifier_provider_request_count(&harness);
        let first = submit_restricted_external_mcp_turn(
            &harness,
            &session_id,
            &workspace_id,
            workspace_root.path(),
            &first_target,
            "harness-mcp-cancel-request-1",
        )
        .await;
        let first_turn_id = first
            .turn_id
            .clone()
            .expect("first MCP Turn should have id");
        let first_task_id = first
            .root_task_id
            .clone()
            .unwrap_or_else(|| panic!("first MCP Task should have id; response={first:?}"));
        let first_pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        harness
            .cancel_with_workspace(&session_id, Some(&workspace_id))
            .await
            .expect("cancelling first MCP approval should succeed");
        let first_turn = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        let first_task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(first_task_id))
            .await;
        harness
            .state
            .runner_manager()
            .expect("MCP cancellation Task should have RunnerManager")
            .quiesce_for_restart(first.root_task_id.as_deref().expect("first root task id"))
            .await;
        assert_eq!(first_turn.status, CanonicalTurnStatus::Cancelled);
        assert!(matches!(
            first_task.status,
            magi_core::TaskStatus::Killed | magi_core::TaskStatus::Failed
        ));
        assert!(!first_target.exists(), "取消 MCP 审批不得产生外部副作用");
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty(),
            "取消后同一 Session 不得遗留 MCP pending approval"
        );
        let first_provider_requests =
            non_classifier_provider_request_count(&harness) - first_requests_before;
        assert_eq!(first_provider_requests, 2);
        record_unified_permission_matrix_row(serde_json::json!({
            "case": "mcp_restricted_cancel_first_turn",
            "tool": "mcp__cancel_mcp__write_file",
            "surface": "mcp_executor",
            "access_profile": "Restricted",
            "scope": "workspace_internal",
            "lifecycle": "cancel",
            "approval_requested": true,
            "approval_resolved": false,
            "provider_requests": first_provider_requests,
            "turn_status": "cancelled",
            "task_status": match first_task.status {
                magi_core::TaskStatus::Killed => "killed",
                magi_core::TaskStatus::Failed => "failed",
                _ => "unexpected",
            },
            "side_effect": "mcp_executor_not_called_file_unchanged",
            "request_id": "harness-mcp-cancel-request-1",
            "turn_id": first_turn_id,
            "execution_profile": "task",
        }));

        let second_requests_before = non_classifier_provider_request_count(&harness);
        let second = submit_restricted_external_mcp_turn(
            &harness,
            &session_id,
            &workspace_id,
            workspace_root.path(),
            &second_target,
            "harness-mcp-cancel-request-2",
        )
        .await;
        let second_turn_id = second
            .turn_id
            .clone()
            .expect("second MCP Turn should have id");
        let second_task_id = second
            .root_task_id
            .clone()
            .unwrap_or_else(|| panic!("second MCP Task should have id; response={second:?}"));
        let second_pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_ne!(
            first_pending.approval_id, second_pending.approval_id,
            "取消第一轮后，第二轮必须创建新的 MCP approval"
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
        let second_turn = tokio::time::timeout(
            Duration::from_secs(3),
            harness.wait_for_terminal(&session_id, &second_turn_id),
        )
        .await
        .expect("MCP approved Turn did not settle");
        let second_task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(second_task_id))
            .await;
        harness
            .state
            .runner_manager()
            .expect("MCP allow-once Task should have RunnerManager")
            .quiesce_for_restart(second.root_task_id.as_deref().expect("second root task id"))
            .await;
        assert_eq!(second_turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(second_task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(&second_target).expect("放行的 MCP Turn 应产生文件"),
            "mcp-cancellation-side-effect\n"
        );
        let second_provider_requests =
            non_classifier_provider_request_count(&harness) - second_requests_before;
        assert_eq!(second_provider_requests, 3);
        record_unified_permission_matrix_row(serde_json::json!({
            "case": "mcp_restricted_allow_after_cancelled_turn",
            "tool": "mcp__cancel_mcp__write_file",
            "surface": "mcp_executor",
            "access_profile": "Restricted",
            "scope": "workspace_internal",
            "lifecycle": "allow_once",
            "approval_requested": true,
            "approval_resolved": true,
            "provider_requests": second_provider_requests,
            "turn_status": "completed",
            "task_status": "completed",
            "side_effect": "mcp_executor_wrote_file",
            "request_id": "harness-mcp-cancel-request-2",
            "turn_id": second_turn_id,
            "execution_profile": "task",
        }));

        let third_requests_before = non_classifier_provider_request_count(&harness);
        let third = submit_restricted_external_mcp_turn(
            &harness,
            &other_session_id,
            &workspace_id,
            workspace_root.path(),
            &third_target,
            "harness-mcp-cancel-request-3",
        )
        .await;
        let third_turn_id = third
            .turn_id
            .clone()
            .expect("third MCP Turn should have id");
        let third_task_id = third
            .root_task_id
            .clone()
            .unwrap_or_else(|| panic!("third MCP Task should have id; response={third:?}"));
        let third_pending = wait_for_pending_tool_approval(&harness, &other_session_id).await;
        assert_ne!(
            second_pending.approval_id, third_pending.approval_id,
            "不同 Session 不得复用 MCP approval"
        );
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty(),
            "Session A 完成后不得影响 Session B 的审批边界"
        );
        harness
            .cancel_with_workspace(&other_session_id, Some(&workspace_id))
            .await
            .expect("cancelling second-session MCP approval should succeed");
        let third_turn = harness
            .wait_for_terminal(&other_session_id, &third_turn_id)
            .await;
        let third_task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(third_task_id))
            .await;
        harness
            .state
            .runner_manager()
            .expect("second-session MCP Task should have RunnerManager")
            .quiesce_for_restart(third.root_task_id.as_deref().expect("third root task id"))
            .await;
        assert_eq!(third_turn.status, CanonicalTurnStatus::Cancelled);
        assert!(matches!(
            third_task.status,
            magi_core::TaskStatus::Killed | magi_core::TaskStatus::Failed
        ));
        assert!(
            !third_target.exists(),
            "Session B 取消 MCP 审批不得产生副作用"
        );
        let third_provider_requests =
            non_classifier_provider_request_count(&harness) - third_requests_before;
        assert_eq!(third_provider_requests, 2);
        assert_eq!(
            executor_calls
                .lock()
                .expect("MCP cancellation executor audit lock should hold")
                .len(),
            1,
            "只有明确放行的第二轮 MCP 调用可以到达 executor"
        );
        record_unified_permission_matrix_row(serde_json::json!({
            "case": "mcp_restricted_cancel_other_session",
            "tool": "mcp__cancel_mcp__write_file",
            "surface": "mcp_executor",
            "access_profile": "Restricted",
            "scope": "workspace_internal",
            "lifecycle": "cancel",
            "approval_requested": true,
            "approval_resolved": false,
            "provider_requests": third_provider_requests,
            "turn_status": "cancelled",
            "task_status": match third_task.status {
                magi_core::TaskStatus::Killed => "killed",
                magi_core::TaskStatus::Failed => "failed",
                _ => "unexpected",
            },
            "side_effect": "mcp_executor_not_called_file_unchanged",
            "request_id": "harness-mcp-cancel-request-3",
            "turn_id": third_turn_id,
            "execution_profile": "task",
        }));
    }

    #[test]
    fn harness_provider_contract_fails_fast_when_expected_tool_is_not_exposed() {
        let provider = HarnessModelClient::new("不会到达最终答复");
        provider.set_tool_then_completed("missing_harness_tool", "{}", "不会到达最终答复");

        let error = provider
            .invoke(ModelInvocationRequest {
                provider: "harness".to_string(),
                prompt: "执行一个工具任务".to_string(),
                messages: None,
                tools: Some(Vec::new()),
                tool_choice: None,
            })
            .expect_err("工具契约不匹配时必须立即失败");

        match error {
            BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: Some(-32_004),
                message,
                ..
            } => {
                assert!(message.contains("missing_harness_tool"));
                assert!(message.contains("not exposed or selected"));
            }
            other => panic!("unexpected harness contract error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn task_profile_permission_block_returns_the_rejection_to_the_model_without_side_effect()
    {
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
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert!(!target.exists(), "只读访问模式下写工具不得产生文件副作用");
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item
                    .tool
                    .as_ref()
                    .is_some_and(|tool| tool.name == "shell_exec")
        }));
        // 单次拒绝不终止任务：拒绝结果交回模型，它改为直接答复，多出一次 Provider 请求。
        assert_eq!(harness.provider.requests().len(), 2);
    }

    #[tokio::test]
    async fn task_profile_repeated_permission_blocks_fail_the_task_after_the_limit() {
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
        harness.provider.set_tool_sequence_then_completed(
            (0..5)
                .map(|index| {
                    (
                        "shell_exec".to_string(),
                        serde_json::json!({
                            "command": format!("printf blocked > {}.{index}", target.display())
                        })
                        .to_string(),
                    )
                })
                .collect::<Vec<_>>(),
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
        // 累计第 3 次被策略拒绝时终止任务，不再请求第 4 次。
        assert_eq!(harness.provider.requests().len(), 3);
    }

    #[tokio::test]
    async fn read_only_profile_rejects_explicit_background_shell_without_side_effect() {
        let harness = MagiTurnHarness::new_task("只读模式拒绝后台进程写入");
        let workspace_root =
            tempfile::tempdir().expect("read-only process workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-read-only-process-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("read-only process workspace should register");
        let session_id = SessionId::new("harness-read-only-process-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "只读后台进程写入验收",
                Some(workspace_id.to_string()),
            )
            .expect("read-only process session should create");
        let target = workspace_root.path().join("read-only-background.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf blocked > {}", target.display()),
                "background": true,
            })
            .to_string(),
            "只读模式不会启动后台进程",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "执行一个任务：调用 shell_exec 在后台写入文件，然后汇总结果",
                "harness-read-only-process-request",
                "harness-read-only-process-user",
                Some(AccessProfile::ReadOnly),
            )
            .await
            .expect("read-only process task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("read-only process should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("read-only process should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert!(!target.exists(), "ReadOnly 后台进程不得产生文件副作用");
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "ReadOnly shell_exec 仍是可用的只读工具面；后台写操作应在工具执行前被拒绝"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "ReadOnly 后台进程拒绝不应发布审批请求"
        );
        record_unified_permission_matrix_row(serde_json::json!({
            "case": "read_only_background_shell",
            "tool": "shell_exec",
            "surface": "background_process",
            "access_profile": "ReadOnly",
            "scope": "workspace_internal",
            "lifecycle": "deny",
            "approval_requested": false,
            "approval_resolved": false,
            "provider_requests": non_classifier_provider_request_count(&harness),
            "turn_status": "failed",
            "task_status": "failed",
            "side_effect": "background_process_not_started_file_unchanged",
            "request_id": "harness-read-only-process-request",
            "turn_id": turn_id,
            "execution_profile": "task",
        }));
    }

    #[tokio::test]
    async fn full_access_profile_allows_background_shell_without_approval() {
        let harness = MagiTurnHarness::new_task("完全授权后台进程完成");
        let workspace_root =
            tempfile::tempdir().expect("full access process workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-full-access-process-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("full access process workspace should register");
        let session_id = SessionId::new("harness-full-access-process-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "完全授权后台进程验收",
                Some(workspace_id.to_string()),
            )
            .expect("full access process session should create");
        let target = workspace_root.path().join("full-access-background.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf full_access_background > {}", target.display()),
                "background": true,
            })
            .to_string(),
            "完全授权后台进程已完成",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 在后台写入文件并返回结果",
                "harness-full-access-process-request",
                "harness-full-access-process-user",
                Some(AccessProfile::FullAccess),
            )
            .await
            .expect("full access process task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("full access process should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("full access process should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !target.exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(&target).expect("full access background output should be readable"),
            "full_access_background"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "完全授权后台进程应执行工具轮和一次最终答复轮"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "完全授权后台进程不应发布审批请求"
        );
        record_unified_permission_matrix_row(serde_json::json!({
            "case": "full_access_background_shell",
            "tool": "shell_exec",
            "surface": "background_process",
            "access_profile": "FullAccess",
            "scope": "workspace_internal",
            "lifecycle": "allow",
            "approval_requested": false,
            "approval_resolved": false,
            "provider_requests": non_classifier_provider_request_count(&harness),
            "turn_status": "completed",
            "task_status": "completed",
            "side_effect": "background_process_started_and_wrote_file",
            "request_id": "harness-full-access-process-request",
            "turn_id": turn_id,
            "execution_profile": "task",
        }));
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
            1,
            "ReadOnly 隐藏 file_write 后只允许一次 Provider 请求收口失败"
        );
        assert_eq!(
            harness.provider.requests().len(),
            1,
            "ReadOnly 隐藏 file_write 后不得进入 Provider 重试循环"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested")
        );
        assert!(
            failed_model_diagnostic_detail(&turn).is_some_and(|detail| {
                detail.contains("expected tool file_write") && detail.contains("was not exposed")
            }),
            "ReadOnly 隐藏 file_write 后应保留 Provider 工具契约诊断"
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "read_only_file_write",
                "tool": "file_write",
                "surface": "file_executor",
                "access_profile": "ReadOnly",
                "scope": "workspace_internal",
                "lifecycle": "deny",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "failed",
                "task_status": "failed",
                "side_effect": "write_blocked",
            }),
            &turn,
        );
    }

    #[tokio::test]
    async fn read_only_profile_rejects_explicit_file_copy_and_move_without_side_effect() {
        let copy_harness = MagiTurnHarness::new_task("只读模式拒绝复制文件");
        let copy_workspace_root =
            tempfile::tempdir().expect("read-only copy workspace should create");
        let copy_workspace_id = magi_core::WorkspaceId::new("harness-read-only-copy-workspace");
        copy_harness
            .state
            .workspace_registry
            .register_native_path(
                copy_workspace_id.clone(),
                copy_workspace_root.path().to_path_buf(),
            )
            .expect("read-only copy workspace should register");
        let copy_session_id = SessionId::new("harness-read-only-copy-session");
        copy_harness
            .state
            .session_store
            .create_session_for_workspace(
                copy_session_id.clone(),
                "只读复制文件验收",
                Some(copy_workspace_id.to_string()),
            )
            .expect("read-only copy session should create");
        let copy_source = copy_workspace_root.path().join("source.txt");
        let copy_destination = copy_workspace_root.path().join("copy.txt");
        fs::write(&copy_source, "copy source").expect("read-only copy fixture should write");
        copy_harness.provider.set_tool_then_completed(
            "file_copy",
            serde_json::json!({
                "source": copy_source.display().to_string(),
                "destination": copy_destination.display().to_string()
            })
            .to_string(),
            "只读模式不会复制文件",
        );

        let copy_response = copy_harness
            .submit_workspace_task_with_access_profile(
                &copy_session_id,
                &copy_workspace_id,
                copy_workspace_root.path(),
                "执行一个任务：调用 file_copy 复制文件，然后汇总结果",
                "harness-read-only-copy-request",
                "harness-read-only-copy-user",
                Some(AccessProfile::ReadOnly),
            )
            .await
            .expect("read-only file copy task should be accepted");
        let copy_turn_id = copy_response
            .turn_id
            .clone()
            .expect("read-only copy should have turn");
        let copy_task_id = copy_response
            .root_task_id
            .clone()
            .expect("read-only copy should have root task");
        let copy_turn = copy_harness
            .wait_for_terminal(&copy_session_id, &copy_turn_id)
            .await;
        let copy_task = copy_harness
            .wait_for_task_terminal(&magi_core::TaskId::new(copy_task_id))
            .await;
        assert_eq!(copy_turn.status, CanonicalTurnStatus::Failed);
        assert_eq!(copy_task.status, magi_core::TaskStatus::Failed);
        assert!(
            !copy_destination.exists(),
            "ReadOnly file_copy 不得产生复制副作用"
        );
        assert_eq!(
            non_classifier_provider_request_count(&copy_harness),
            1,
            "ReadOnly 隐藏 file_copy 后只允许一次 Provider 请求收口失败"
        );
        assert!(
            copy_harness
                .events_for(&copy_session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "ReadOnly file_copy 不应发布审批请求"
        );

        let move_harness = MagiTurnHarness::new_task("只读模式拒绝移动文件");
        let move_workspace_root =
            tempfile::tempdir().expect("read-only move workspace should create");
        let move_workspace_id = magi_core::WorkspaceId::new("harness-read-only-move-workspace");
        move_harness
            .state
            .workspace_registry
            .register_native_path(
                move_workspace_id.clone(),
                move_workspace_root.path().to_path_buf(),
            )
            .expect("read-only move workspace should register");
        let move_session_id = SessionId::new("harness-read-only-move-session");
        move_harness
            .state
            .session_store
            .create_session_for_workspace(
                move_session_id.clone(),
                "只读移动文件验收",
                Some(move_workspace_id.to_string()),
            )
            .expect("read-only move session should create");
        let move_source = move_workspace_root.path().join("source.txt");
        let move_destination = move_workspace_root.path().join("moved.txt");
        fs::write(&move_source, "move source").expect("read-only move fixture should write");
        move_harness.provider.set_tool_then_completed(
            "file_move",
            serde_json::json!({
                "source": move_source.display().to_string(),
                "destination": move_destination.display().to_string()
            })
            .to_string(),
            "只读模式不会移动文件",
        );

        let move_response = move_harness
            .submit_workspace_task_with_access_profile(
                &move_session_id,
                &move_workspace_id,
                move_workspace_root.path(),
                "执行一个任务：调用 file_move 移动文件，然后汇总结果",
                "harness-read-only-move-request",
                "harness-read-only-move-user",
                Some(AccessProfile::ReadOnly),
            )
            .await
            .expect("read-only file move task should be accepted");
        let move_turn_id = move_response
            .turn_id
            .clone()
            .expect("read-only move should have turn");
        let move_task_id = move_response
            .root_task_id
            .clone()
            .expect("read-only move should have root task");
        let move_turn = move_harness
            .wait_for_terminal(&move_session_id, &move_turn_id)
            .await;
        let move_task = move_harness
            .wait_for_task_terminal(&magi_core::TaskId::new(move_task_id))
            .await;
        assert_eq!(move_turn.status, CanonicalTurnStatus::Failed);
        assert_eq!(move_task.status, magi_core::TaskStatus::Failed);
        assert!(move_source.exists(), "ReadOnly file_move 不得删除源文件");
        assert!(
            !move_destination.exists(),
            "ReadOnly file_move 不得创建目标文件"
        );
        assert_eq!(
            non_classifier_provider_request_count(&move_harness),
            1,
            "ReadOnly 隐藏 file_move 后只允许一次 Provider 请求收口失败"
        );
        assert!(
            move_harness
                .events_for(&move_session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "ReadOnly file_move 不应发布审批请求"
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "read_only_file_copy",
                "tool": "file_copy",
                "surface": "file_executor",
                "access_profile": "ReadOnly",
                "scope": "workspace_internal",
                "lifecycle": "deny",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&copy_harness),
                "turn_status": "failed",
                "task_status": "failed",
                "side_effect": "write_blocked",
            }),
            &copy_turn,
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "read_only_file_move",
                "tool": "file_move",
                "surface": "file_executor",
                "access_profile": "ReadOnly",
                "scope": "workspace_internal",
                "lifecycle": "deny",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&move_harness),
                "turn_status": "failed",
                "task_status": "failed",
                "side_effect": "write_blocked",
            }),
            &move_turn,
        );
    }

    #[tokio::test]
    async fn read_only_profile_rejects_explicit_file_patch_mkdir_and_remove_without_side_effect() {
        let (_patch_workspace, patch_target, patch_turn, _patch_task) =
            run_read_only_explicit_file_tool_case(
                "file_patch",
                "file-patch",
                "执行一个任务：调用 file_patch 修改工作区文件，然后汇总结果".to_string(),
                |workspace_root| {
                    let target = workspace_root.join("read-only-patch.txt");
                    fs::write(&target, "before\n").expect("read-only patch fixture should write");
                    (
                        target.clone(),
                        serde_json::json!({
                            "path": target.display().to_string(),
                            "old_string": "before",
                            "new_string": "after"
                        }),
                    )
                },
            )
            .await;
        assert_eq!(
            fs::read_to_string(&patch_target).expect("read-only patch target should remain"),
            "before\n",
            "ReadOnly file_patch 不得产生文件副作用"
        );
        assert!(
            failed_model_diagnostic_detail(&patch_turn).is_some_and(|detail| {
                detail.contains("expected tool file_patch") && detail.contains("was not exposed")
            }),
            "ReadOnly file_patch 应保留 fail-closed 工具契约诊断"
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "read_only_file-patch",
                "tool": "file_patch",
                "surface": "file_executor",
                "access_profile": "ReadOnly",
                "scope": "workspace_internal",
                "lifecycle": "deny",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": 1,
                "turn_status": "failed",
                "task_status": "failed",
                "side_effect": "write_blocked",
            }),
            &patch_turn,
        );

        let (_mkdir_workspace, mkdir_target, mkdir_turn, _mkdir_task) =
            run_read_only_explicit_file_tool_case(
                "file_mkdir",
                "file-mkdir",
                "执行一个任务：调用 file_mkdir 创建工作区目录，然后汇总结果".to_string(),
                |workspace_root| {
                    let target = workspace_root.join("read-only-mkdir").join("nested");
                    (
                        target.clone(),
                        serde_json::json!({"path": target.display().to_string()}),
                    )
                },
            )
            .await;
        assert!(!mkdir_target.exists(), "ReadOnly file_mkdir 不得创建目录");
        assert!(
            failed_model_diagnostic_detail(&mkdir_turn).is_some_and(|detail| {
                detail.contains("expected tool file_mkdir") && detail.contains("was not exposed")
            }),
            "ReadOnly file_mkdir 应保留 fail-closed 工具契约诊断"
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "read_only_file-mkdir",
                "tool": "file_mkdir",
                "surface": "file_executor",
                "access_profile": "ReadOnly",
                "scope": "workspace_internal",
                "lifecycle": "deny",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": 1,
                "turn_status": "failed",
                "task_status": "failed",
                "side_effect": "write_blocked",
            }),
            &mkdir_turn,
        );

        let (_remove_workspace, remove_target, remove_turn, _remove_task) =
            run_read_only_explicit_file_tool_case(
                "file_remove",
                "file-remove",
                "执行一个任务：调用 file_remove 删除工作区文件，然后汇总结果".to_string(),
                |workspace_root| {
                    let target = workspace_root.join("read-only-remove.txt");
                    fs::write(&target, "must remain")
                        .expect("read-only remove fixture should write");
                    (
                        target.clone(),
                        serde_json::json!({"path": target.display().to_string()}),
                    )
                },
            )
            .await;
        assert!(remove_target.exists(), "ReadOnly file_remove 不得删除文件");
        assert_eq!(
            fs::read_to_string(&remove_target).expect("read-only remove target should remain"),
            "must remain"
        );
        assert!(
            failed_model_diagnostic_detail(&remove_turn).is_some_and(|detail| {
                detail.contains("expected tool file_remove") && detail.contains("was not exposed")
            }),
            "ReadOnly file_remove 应保留 fail-closed 工具契约诊断"
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "read_only_file-remove",
                "tool": "file_remove",
                "surface": "file_executor",
                "access_profile": "ReadOnly",
                "scope": "workspace_internal",
                "lifecycle": "deny",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": 1,
                "turn_status": "failed",
                "task_status": "failed",
                "side_effect": "write_blocked",
            }),
            &remove_turn,
        );
    }

    #[tokio::test]
    async fn read_only_profile_rejects_explicit_apply_patch_without_side_effect() {
        let (_workspace, target, turn, _task) = run_read_only_explicit_file_tool_case(
            "apply_patch",
            "apply-patch",
            "执行一个任务：调用 apply_patch 新建工作区文件，然后汇总结果".to_string(),
            |workspace_root| {
                let target = workspace_root.join("read-only-apply-patch.txt");
                let patch = "*** Begin Patch\n*** Add File: read-only-apply-patch.txt\n+must not create\n*** End Patch\n";
                (
                    target,
                    serde_json::json!({"patch": patch}),
                )
            },
        )
        .await;
        assert!(!target.exists(), "ReadOnly apply_patch 不得创建文件");
        assert!(
            failed_model_diagnostic_detail(&turn).is_some_and(|detail| {
                detail.contains("expected tool apply_patch") && detail.contains("was not exposed")
            }),
            "ReadOnly apply_patch 应保留 fail-closed 工具契约诊断"
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "read_only_apply-patch",
                "tool": "apply_patch",
                "surface": "file_executor",
                "access_profile": "ReadOnly",
                "scope": "workspace_internal",
                "lifecycle": "deny",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": 1,
                "turn_status": "failed",
                "task_status": "failed",
                "side_effect": "write_blocked",
            }),
            &turn,
        );
    }

    #[tokio::test]
    async fn read_only_profile_rejects_explicit_image_and_git_writes() {
        for (tool_name, request_suffix, prompt, arguments) in [
            (
                "image_generate",
                "image-generate",
                "执行一个任务：调用 image_generate 生成文件，然后汇总结果",
                serde_json::json!({
                    "prompt": "must not generate",
                    "output_path": "read-only-image.png"
                }),
            ),
            (
                "git_push",
                "git-push",
                "执行一个任务：调用 git_push 推送分支，然后汇总结果",
                serde_json::json!({"remote": "origin", "branch": "blocked"}),
            ),
            (
                "git_branch_switch",
                "git-branch-switch",
                "执行一个任务：调用 git_branch_switch 切换分支，然后汇总结果",
                serde_json::json!({"branch": "approval-target"}),
            ),
        ] {
            let (_workspace, target, turn, task) = run_read_only_explicit_file_tool_case(
                tool_name,
                request_suffix,
                prompt.to_string(),
                |workspace_root| {
                    let target = workspace_root.join(format!("{request_suffix}.marker"));
                    (target, arguments.clone())
                },
            )
            .await;
            assert!(!target.exists(), "ReadOnly {tool_name} 不得产生文件副作用");
            assert_eq!(turn.status, CanonicalTurnStatus::Failed, "tool={tool_name}");
            assert_eq!(
                task.status,
                magi_core::TaskStatus::Failed,
                "tool={tool_name}"
            );
        }
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
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "read_only_file_read",
                "tool": "file_read",
                "surface": "file_executor",
                "access_profile": "ReadOnly",
                "scope": "workspace_internal",
                "lifecycle": "allow",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "file_read",
            }),
            &turn,
        );
    }

    #[tokio::test]
    async fn read_only_profile_allows_explicit_read_only_shell_without_approval() {
        let harness = MagiTurnHarness::new_task("只读 Shell 读取完成");
        let workspace_root = tempfile::tempdir().expect("read-only shell workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-read-only-shell-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("read-only shell workspace should register");
        let session_id = SessionId::new("harness-read-only-shell-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "只读 Shell 读取验收",
                Some(workspace_id.to_string()),
            )
            .expect("read-only shell session should create");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": "printf READONLY_SHELL_OK",
                "access_mode": "read_only"
            })
            .to_string(),
            "只读 Shell 读取完成",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "在只读模式下明确调用 shell_exec，以 read_only 执行读取命令，然后汇总结果",
                "harness-read-only-shell-request",
                "harness-read-only-shell-user",
                Some(AccessProfile::ReadOnly),
            )
            .await
            .expect("read-only shell task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("read-only shell task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("read-only shell task should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;

        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "ReadOnly read-only shell 不应进入审批流程"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "shell_exec"
                        && tool
                            .result
                            .as_ref()
                            .is_some_and(|result| result.to_string().contains("READONLY_SHELL_OK"))
                        && tool.error.is_none()
                })
        }));
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "read_only_shell_read",
                "tool": "shell_exec",
                "surface": "shell_executor",
                "access_profile": "ReadOnly",
                "scope": "workspace_internal",
                "lifecycle": "allow",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "stdout_only",
            }),
            &turn,
        );
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
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "full_access_shell_write",
                "tool": "shell_exec",
                "surface": "shell_executor",
                "access_profile": "FullAccess",
                "scope": "workspace_internal",
                "lifecycle": "allow",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "file_written",
            }),
            &turn,
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
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "restricted_shell_write_allow_once",
                "tool": "shell_exec",
                "surface": "shell_executor",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "allow_once",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "file_written",
            }),
            &turn,
        );
    }

    fn record_unified_permission_matrix_row(mut row: serde_json::Value) {
        let fixture_id = format!(
            "turn-service-api:{}:{}:{}:{}",
            row["surface"].as_str().unwrap_or("unknown"),
            row["tool"].as_str().unwrap_or("unknown"),
            row["access_profile"].as_str().unwrap_or("unknown"),
            row["case"].as_str().unwrap_or("unknown"),
        );
        row["fixture_id"] = serde_json::Value::String(fixture_id);
        row["schema_version"] = serde_json::Value::String("magi.permission.fixture.v1".to_string());
        row["terminal_source"] = row
            .get("terminal_source")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::String("canonical_turn_coordinator".to_string()));
        const REQUIRED_STRING_FIELDS: &[&str] = &[
            "case",
            "tool",
            "surface",
            "access_profile",
            "scope",
            "lifecycle",
            "turn_status",
            "task_status",
            "side_effect",
        ];
        for field in REQUIRED_STRING_FIELDS {
            assert!(
                row.get(*field)
                    .and_then(serde_json::Value::as_str)
                    .is_some(),
                "permission matrix row must contain string field {field}: {row}"
            );
        }
        assert!(
            row.get("approval_requested")
                .and_then(serde_json::Value::as_bool)
                .is_some(),
            "permission matrix row must contain boolean approval_requested: {row}"
        );
        assert!(
            row.get("approval_resolved")
                .and_then(serde_json::Value::as_bool)
                .is_some(),
            "permission matrix row must contain boolean approval_resolved: {row}"
        );
        assert!(
            row.get("provider_requests")
                .and_then(serde_json::Value::as_u64)
                .is_some(),
            "permission matrix row must contain numeric provider_requests: {row}"
        );
        for field in ["request_id", "turn_id", "execution_profile"] {
            assert!(
                row.get(field)
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| !value.trim().is_empty()),
                "Turn permission matrix row must contain {field}: {row}"
            );
        }
        assert_eq!(
            row["execution_profile"].as_str(),
            Some("task"),
            "tool permission matrix rows must come from a task-profile Turn: {row}"
        );
        static ROWS: OnceLock<Mutex<Vec<serde_json::Value>>> = OnceLock::new();
        let rows = ROWS.get_or_init(|| Mutex::new(Vec::new()));
        let mut rows = rows
            .lock()
            .expect("unified permission matrix lock should hold");
        let case_name = row["case"].as_str();
        let tool_name = row["tool"].as_str();
        let surface = row["surface"].as_str();
        rows.retain(|existing| {
            !(existing["case"].as_str() == case_name
                && existing["tool"].as_str() == tool_name
                && existing["surface"].as_str() == surface)
        });
        rows.push(row);
        rows.sort_by(|left, right| {
            left["tool"]
                .as_str()
                .cmp(&right["tool"].as_str())
                .then_with(|| left["case"].as_str().cmp(&right["case"].as_str()))
        });
        let evidence_path = std::env::var_os("MAGI_API_PERMISSION_MATRIX_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp/magi-api-permission-matrix.json"));
        fs::write(
            evidence_path,
            serde_json::to_vec_pretty(&*rows)
                .expect("unified permission matrix rows should serialize"),
        )
        .expect("unified permission matrix evidence should write");
    }

    fn record_turn_permission_matrix_row(mut row: serde_json::Value, turn: &CanonicalTurn) {
        let user_message = turn
            .items
            .iter()
            .find(|item| item.kind == CanonicalTurnItemKind::UserMessage);
        let metadata_value = |key: &str, snake_key: &str| {
            turn.metadata
                .get(key)
                .or_else(|| turn.metadata.get(snake_key))
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    user_message.and_then(|item| {
                        item.metadata
                            .get(key)
                            .or_else(|| item.metadata.get(snake_key))
                            .and_then(serde_json::Value::as_str)
                    })
                })
        };
        let request_id = metadata_value("requestId", "request_id")
            .filter(|value| !value.trim().is_empty())
            .expect("permission matrix Turn must carry its canonical request identity");
        let execution_profile = metadata_value("executionProfile", "execution_profile")
            .filter(|value| !value.trim().is_empty())
            .expect("permission matrix Turn must carry its canonical execution profile");

        for (field, expected) in [
            ("request_id", request_id),
            ("turn_id", turn.turn_id.as_str()),
            ("execution_profile", execution_profile),
        ] {
            if let Some(actual) = row.get(field).and_then(serde_json::Value::as_str) {
                assert_eq!(
                    actual, expected,
                    "permission matrix {field} must match canonical Turn"
                );
            }
            row[field] = serde_json::Value::String(expected.to_string());
        }
        record_unified_permission_matrix_row(row);
    }

    #[allow(clippy::too_many_arguments)]
    fn record_process_approval_matrix_row(
        case_name: &str,
        lifecycle: &str,
        side_effect: &str,
        approval_requested: bool,
        approval_resolved: bool,
        provider_requests: usize,
        turn_status: &str,
        task_status: &str,
        turn_id: Option<&str>,
        request_id: Option<&str>,
    ) {
        static ROWS: OnceLock<Mutex<Vec<serde_json::Value>>> = OnceLock::new();
        let rows = ROWS.get_or_init(|| Mutex::new(Vec::new()));
        let mut rows = rows.lock().expect("process matrix rows lock should hold");
        rows.retain(|row| row["case"] != case_name);
        let mut row = serde_json::json!({
            "case": case_name,
            "tool": "shell_exec",
            "surface": "background_process",
            "access_profile": "Restricted",
            "scope": "workspace_internal",
            "lifecycle": lifecycle,
            "approval_requested": approval_requested,
            "approval_resolved": approval_resolved,
            "provider_requests": provider_requests,
            "turn_status": turn_status,
            "task_status": task_status,
            "side_effect": side_effect,
            "execution_profile": "task",
        });
        if let Some(turn_id) = turn_id {
            row["turn_id"] = serde_json::Value::String(turn_id.to_string());
        }
        if let Some(request_id) = request_id {
            row["request_id"] = serde_json::Value::String(request_id.to_string());
        }
        rows.push(row.clone());
        rows.sort_by(|left, right| left["case"].as_str().cmp(&right["case"].as_str()));
        record_unified_permission_matrix_row(row);
        let path = std::env::var_os("MAGI_API_PROCESS_APPROVAL_MATRIX_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp/magi-api-process-approval-matrix.json"));
        fs::write(
            path,
            serde_json::to_vec_pretty(&*rows).expect("process matrix rows should serialize"),
        )
        .expect("process matrix evidence should write");
    }

    async fn run_restricted_background_shell_approval_case(
        case_name: &'static str,
        decision: &'static str,
    ) {
        let harness = MagiTurnHarness::new_task(format!("后台进程审批-{case_name}"));
        let workspace_root = tempfile::tempdir().expect("process approval workspace should create");
        let workspace_id =
            magi_core::WorkspaceId::new(format!("harness-process-approval-{case_name}-workspace"));
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("process approval workspace should register");
        let session_id = SessionId::new(format!("harness-process-approval-{case_name}-session"));
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                format!("后台进程审批验收-{case_name}"),
                Some(workspace_id.to_string()),
            )
            .expect("process approval session should create");
        let target = workspace_root
            .path()
            .join(format!("background-{case_name}.txt"));
        let content = format!("background-{case_name}");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf {content} > {}", target.display()),
                "background": true,
            })
            .to_string(),
            format!("后台进程审批 {case_name} 已收口"),
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                &format!("调用 shell_exec 在后台启动进程，执行 {case_name} 审批验收"),
                &format!("harness-process-approval-{case_name}-request"),
                &format!("harness-process-approval-{case_name}-user"),
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("restricted background process task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("background process task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("background process task should have root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "shell_exec");
        match decision {
            "allow_once" | "deny" => {
                resolve_tool_approval_via_http(
                    &harness,
                    &session_id,
                    &workspace_id,
                    workspace_root.path(),
                    &pending.approval_id,
                    decision,
                )
                .await;
            }
            "cancel" => {
                harness
                    .cancel_with_workspace(&session_id, Some(&workspace_id))
                    .await
                    .expect("取消后台进程审批应成功");
            }
            "expiry" => {
                assert_eq!(
                    harness
                        .state
                        .turn_coordinator()
                        .tool_approvals()
                        .expire_stale(
                            UtcMillis(UtcMillis::now().0 + TOOL_APPROVAL_TTL_MILLIS + 1,)
                        ),
                    1,
                    "测试必须显式推进后台进程审批时钟"
                );
            }
            other => panic!("未知后台进程审批决定: {other}"),
        }

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        // 拒绝或过期只结束这次调用，模型拿到结果后完成本轮（P1-9）。
        let expected_turn_status = if decision == "cancel" {
            CanonicalTurnStatus::Cancelled
        } else {
            CanonicalTurnStatus::Completed
        };
        assert_eq!(turn.status, expected_turn_status);
        if decision == "allow_once" {
            assert_eq!(task.status, magi_core::TaskStatus::Completed);
            let deadline = Instant::now() + Duration::from_secs(2);
            while !target.exists() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(
                fs::read_to_string(&target).expect("background process write should be readable"),
                content
            );
        } else {
            assert_eq!(
                task.status,
                if decision == "cancel" {
                    magi_core::TaskStatus::Killed
                } else {
                    magi_core::TaskStatus::Completed
                }
            );
            assert!(!target.exists(), "后台进程审批未放行不得产生文件副作用");
        }
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .any(|event| event.event_type == "tool.approval.requested")
        );
        let approval_resolved = harness
            .events_for(&session_id)
            .iter()
            .any(|event| event.event_type == "tool.approval.resolved");
        assert_eq!(approval_resolved, matches!(decision, "allow_once" | "deny"));
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            if decision == "cancel" { 1 } else { 2 }
        );
        record_process_approval_matrix_row(
            case_name,
            decision,
            if decision == "allow_once" {
                "background_process_started_and_wrote_file"
            } else {
                "background_process_not_started_file_unchanged"
            },
            true,
            approval_resolved,
            non_classifier_provider_request_count(&harness),
            match turn.status {
                CanonicalTurnStatus::Completed => "completed",
                CanonicalTurnStatus::Cancelled => "cancelled",
                CanonicalTurnStatus::Failed => "failed",
                _ => "unexpected",
            },
            match task.status {
                magi_core::TaskStatus::Completed => "completed",
                magi_core::TaskStatus::Failed => "failed",
                magi_core::TaskStatus::Killed => "killed",
                _ => "unexpected",
            },
            Some(&turn_id),
            Some(&format!("harness-process-approval-{case_name}-request")),
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_background_shell_approval_runs_real_process_once() {
        run_restricted_background_shell_approval_case("allow_once", "allow_once").await;
    }

    #[tokio::test]
    async fn restricted_profile_background_shell_approval_denial_preserves_side_effect_boundary() {
        run_restricted_background_shell_approval_case("deny", "deny").await;
    }

    #[tokio::test]
    async fn restricted_profile_background_shell_approval_cancel_preserves_side_effect_boundary() {
        run_restricted_background_shell_approval_case("cancel", "cancel").await;
    }

    #[tokio::test]
    async fn restricted_profile_background_shell_approval_expiry_preserves_side_effect_boundary() {
        run_restricted_background_shell_approval_case("expiry", "expiry").await;
    }

    #[tokio::test]
    async fn restricted_profile_background_shell_duplicate_replays_pending_approval() {
        let harness = MagiTurnHarness::new_task("后台进程审批重复回放");
        let workspace_root =
            tempfile::tempdir().expect("duplicate process workspace should create");
        let workspace_id =
            magi_core::WorkspaceId::new("harness-process-approval-duplicate-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("duplicate process workspace should register");
        let session_id = SessionId::new("harness-process-approval-duplicate-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "后台进程审批重复回放验收",
                Some(workspace_id.to_string()),
            )
            .expect("duplicate process session should create");
        let target = workspace_root.path().join("background-duplicate.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf duplicate > {}", target.display()),
                "background": true,
            })
            .to_string(),
            "后台进程审批重复请求最终完成",
        );

        let first = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 在后台写入文件，等待审批后汇总结果",
                "harness-process-approval-duplicate-request",
                "harness-process-approval-duplicate-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("重复后台进程审批首次请求应被接纳");
        let first_turn_id = first.turn_id.clone().expect("首次请求应有 Turn");
        let first_task_id = first.root_task_id.clone().expect("首次请求应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;

        let replay = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 在后台写入文件，等待审批后汇总结果",
                "harness-process-approval-duplicate-request",
                "harness-process-approval-duplicate-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("重复后台进程审批请求应回放原 Turn");
        assert_eq!(replay.turn_id.as_deref(), Some(first_turn_id.as_str()));
        assert_eq!(replay.root_task_id.as_deref(), Some(first_task_id.as_str()));
        assert_eq!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .len(),
            1,
            "重复后台进程请求不得创建第二个 pending 审批"
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 1);

        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            workspace_root.path(),
            &pending.approval_id,
            "allow_once",
        )
        .await;
        let turn = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(first_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(&target).expect("重复后台进程放行后文件应存在"),
            "duplicate"
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            1
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "background_process_duplicate",
                "tool": "shell_exec",
                "surface": "background_process",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "duplicate_pending",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "background_process_started_once",
                "request_id": "harness-process-approval-duplicate-request",
                "turn_id": first_turn_id,
                "execution_profile": "task",
            }),
            &turn,
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_background_shell_allow_for_turn_does_not_cross_turn_or_session() {
        let harness = MagiTurnHarness::new_task("后台进程 allow_for_turn 隔离");
        let workspace_root =
            tempfile::tempdir().expect("process cross-scope workspace should create");
        let workspace_id =
            magi_core::WorkspaceId::new("harness-process-approval-cross-scope-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("process cross-scope workspace should register");
        let session_id = SessionId::new("harness-process-approval-cross-scope-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "后台进程 allow_for_turn 隔离会话",
                Some(workspace_id.to_string()),
            )
            .expect("process cross-scope session should create");

        let first_requests_before = non_classifier_provider_request_count(&harness);
        let first_target = workspace_root.path().join("process-cross-turn-first.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf first > {}", first_target.display()),
                "background": true,
            })
            .to_string(),
            "后台进程第一轮完成",
        );
        let first = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "执行一个任务：调用 shell_exec 第一轮启动后台进程并授予本 Turn 权限",
                "harness-process-cross-scope-request-1",
                "harness-process-cross-scope-user-1",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("后台进程第一轮应被接纳");
        let first_turn_id = first.turn_id.clone().expect("后台进程第一轮应有 Turn");
        let first_task_id = first
            .root_task_id
            .clone()
            .expect("后台进程第一轮应有 root task");
        let first_pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            workspace_root.path(),
            &first_pending.approval_id,
            "allow_for_turn",
        )
        .await;
        let first_turn = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        let first_task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(first_task_id))
            .await;
        assert_eq!(first_turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(first_task.status, magi_core::TaskStatus::Completed);
        let first_deadline = Instant::now() + Duration::from_secs(2);
        while !first_target.exists() && Instant::now() < first_deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            fs::read_to_string(&first_target).expect("第一轮后台进程文件应存在"),
            "first"
        );
        let first_provider_requests =
            non_classifier_provider_request_count(&harness) - first_requests_before;
        assert_eq!(first_provider_requests, 2);
        record_process_approval_matrix_row(
            "background_process_allow_for_turn_initial_turn",
            "allow_for_turn",
            "first_background_process_started_file_written",
            true,
            true,
            first_provider_requests,
            "completed",
            "completed",
            Some(&first_turn_id),
            Some("harness-process-cross-scope-request-1"),
        );

        let second_requests_before = non_classifier_provider_request_count(&harness);
        let second_target = workspace_root.path().join("process-cross-turn-second.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf second > {}", second_target.display()),
                "background": true,
            })
            .to_string(),
            "后台进程第二轮完成",
        );
        let second = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "执行一个任务：调用 shell_exec 第二轮启动后台进程，必须重新审批",
                "harness-process-cross-scope-request-2",
                "harness-process-cross-scope-user-2",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("后台进程第二轮应被接纳");
        let second_turn_id = second.turn_id.clone().expect("后台进程第二轮应有 Turn");
        let second_task_id = second
            .root_task_id
            .clone()
            .expect("后台进程第二轮应有 root task");
        let second_pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_ne!(
            first_pending.approval_id, second_pending.approval_id,
            "后台进程 allow_for_turn 不得跨 Turn 复用"
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
        let second_turn = harness
            .wait_for_terminal(&session_id, &second_turn_id)
            .await;
        let second_task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(second_task_id))
            .await;
        assert_eq!(second_turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(second_task.status, magi_core::TaskStatus::Completed);
        let second_deadline = Instant::now() + Duration::from_secs(2);
        while !second_target.exists() && Instant::now() < second_deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            fs::read_to_string(&second_target).expect("第二轮后台进程文件应存在"),
            "second"
        );
        let second_provider_requests =
            non_classifier_provider_request_count(&harness) - second_requests_before;
        assert_eq!(second_provider_requests, 2);
        record_process_approval_matrix_row(
            "background_process_reapproval_after_turn_grant",
            "fresh_allow_once_after_turn_grant",
            "second_background_process_started_after_new_approval",
            true,
            true,
            second_provider_requests,
            "completed",
            "completed",
            Some(&second_turn_id),
            Some("harness-process-cross-scope-request-2"),
        );

        let third_requests_before = non_classifier_provider_request_count(&harness);
        let other_session_id = SessionId::new("harness-process-approval-cross-scope-other-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                other_session_id.clone(),
                "后台进程 allow_for_turn 新 Session",
                Some(workspace_id.to_string()),
            )
            .expect("process cross-scope other session should create");
        let third_target = workspace_root.path().join("process-cross-session.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf third > {}", third_target.display()),
                "background": true,
            })
            .to_string(),
            "后台进程新 Session 完成",
        );
        let third = harness
            .submit_workspace_task_with_access_profile(
                &other_session_id,
                &workspace_id,
                workspace_root.path(),
                "执行一个任务：调用 shell_exec 新 Session 启动后台进程，必须独立审批",
                "harness-process-cross-scope-request-3",
                "harness-process-cross-scope-user-3",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("后台进程新 Session 应被接纳");
        let third_turn_id = third.turn_id.clone().expect("后台进程新 Session 应有 Turn");
        let third_task_id = third
            .root_task_id
            .clone()
            .expect("后台进程新 Session 应有 root task");
        let third_pending = wait_for_pending_tool_approval(&harness, &other_session_id).await;
        assert_ne!(
            second_pending.approval_id, third_pending.approval_id,
            "后台进程 allow_for_turn 不得跨 Session 复用"
        );
        resolve_tool_approval_via_http(
            &harness,
            &other_session_id,
            &workspace_id,
            workspace_root.path(),
            &third_pending.approval_id,
            "allow_once",
        )
        .await;
        let third_turn = harness
            .wait_for_terminal(&other_session_id, &third_turn_id)
            .await;
        let third_task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(third_task_id))
            .await;
        assert_eq!(third_turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(third_task.status, magi_core::TaskStatus::Completed);
        let third_deadline = Instant::now() + Duration::from_secs(2);
        while !third_target.exists() && Instant::now() < third_deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            fs::read_to_string(&third_target).expect("跨 Session 后台进程文件应存在"),
            "third"
        );
        let third_provider_requests =
            non_classifier_provider_request_count(&harness) - third_requests_before;
        assert_eq!(third_provider_requests, 2);
        record_process_approval_matrix_row(
            "background_process_reapproval_in_new_session",
            "fresh_allow_once_after_session_change",
            "third_background_process_started_after_new_approval",
            true,
            true,
            third_provider_requests,
            "completed",
            "completed",
            Some(&third_turn_id),
            Some("harness-process-cross-scope-request-3"),
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            2
        );
        assert_eq!(
            harness
                .events_for(&other_session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 6);
        let _ = fs::remove_dir_all(workspace_root);
    }

    fn prepare_git_approval_case(
        label: &str,
        title: &str,
    ) -> (MagiTurnHarness, magi_core::WorkspaceId, PathBuf, SessionId) {
        let harness = MagiTurnHarness::new_task(title);
        let (workspace_id, workspace_root) = register_git_workspace(&harness);
        git_fixture_command(&workspace_root, &["switch", "-c", "approval-target"]);
        git_fixture_command(&workspace_root, &["switch", "main"]);
        let session_id = SessionId::new(format!("harness-git-approval-{label}-session"));
        harness
            .state
            .session_store
            .create_session_for_workspace(session_id.clone(), title, Some(workspace_id.to_string()))
            .expect("Git 审批 session 应创建");
        (harness, workspace_id, workspace_root, session_id)
    }

    fn current_git_branch(workspace_root: &Path) -> String {
        let output = magi_process::std_command("git")
            .arg("-C")
            .arg(workspace_root)
            .args(["branch", "--show-current"])
            .output()
            .expect("Git 当前分支查询应启动");
        assert!(
            output.status.success(),
            "Git 当前分支查询失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn current_git_head(workspace_root: &Path) -> String {
        let output = magi_process::std_command("git")
            .arg("-C")
            .arg(workspace_root)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("Git HEAD 查询应启动");
        assert!(
            output.status.success(),
            "Git HEAD 查询失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn git_worktree_paths(workspace_root: &Path) -> Vec<PathBuf> {
        let output = magi_process::std_command("git")
            .arg("-C")
            .arg(workspace_root)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .expect("Git worktree 查询应启动");
        assert!(
            output.status.success(),
            "Git worktree 查询失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(PathBuf::from)
            .collect()
    }

    fn git_worktree_count(workspace_root: &Path) -> usize {
        git_worktree_paths(workspace_root).len()
    }

    fn same_git_path(left: &Path, right: &Path) -> bool {
        match (fs::canonicalize(left), fs::canonicalize(right)) {
            (Ok(left), Ok(right)) => left == right,
            _ => left == right,
        }
    }

    fn git_branch_exists(workspace_root: &Path, branch: &str) -> bool {
        let output = magi_process::std_command("git")
            .arg("-C")
            .arg(workspace_root)
            .args(["branch", "--list", branch])
            .output()
            .expect("Git branch existence query should start");
        assert!(
            output.status.success(),
            "Git branch existence query failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        !String::from_utf8_lossy(&output.stdout).trim().is_empty()
    }

    fn remote_git_branch_exists(remote_root: &Path, branch: &str) -> bool {
        let reference = format!("refs/heads/{branch}");
        magi_process::std_command("git")
            .args([
                "--git-dir",
                remote_root.to_string_lossy().as_ref(),
                "show-ref",
                "--verify",
                "--quiet",
            ])
            .arg(reference)
            .status()
            .is_ok_and(|status| status.success())
    }

    fn prepare_git_push_approval_case(
        label: &str,
        title: &str,
    ) -> (
        MagiTurnHarness,
        magi_core::WorkspaceId,
        PathBuf,
        SessionId,
        tempfile::TempDir,
    ) {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case(label, title);
        let remote_root = tempfile::tempdir().expect("Git push bare remote should create");
        git_fixture_command(remote_root.path(), &["init", "--bare"]);
        let remote_path = remote_root.path().to_string_lossy().to_string();
        git_fixture_command(
            &workspace_root,
            &["remote", "add", "origin", remote_path.as_str()],
        );
        (
            harness,
            workspace_id,
            workspace_root,
            session_id,
            remote_root,
        )
    }

    fn prepare_git_pull_approval_case(
        label: &str,
        title: &str,
    ) -> (
        MagiTurnHarness,
        magi_core::WorkspaceId,
        PathBuf,
        SessionId,
        tempfile::TempDir,
        tempfile::TempDir,
        String,
    ) {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case(label, title);
        let remote_root = tempfile::tempdir().expect("Git pull bare remote should create");
        git_fixture_command(remote_root.path(), &["init", "--bare"]);
        let remote_path = remote_root.path().to_string_lossy().to_string();
        git_fixture_command(
            &workspace_root,
            &["remote", "add", "origin", remote_path.as_str()],
        );
        git_fixture_command(
            &workspace_root,
            &["push", "--set-upstream", "origin", "main"],
        );
        git_fixture_command(
            remote_root.path(),
            &["symbolic-ref", "HEAD", "refs/heads/main"],
        );
        let initial_head = current_git_head(&workspace_root);

        let peer_parent = tempfile::tempdir().expect("Git pull peer parent should create");
        let peer_root = peer_parent.path().join("peer");
        let peer_path = peer_root.to_string_lossy().to_string();
        git_fixture_command(
            peer_parent.path(),
            &[
                "clone",
                "--branch",
                "main",
                remote_path.as_str(),
                peer_path.as_str(),
            ],
        );
        git_fixture_command(&peer_root, &["config", "user.name", "Magi Pull Peer"]);
        git_fixture_command(
            &peer_root,
            &["config", "user.email", "magi-pull-peer@example.test"],
        );
        fs::write(peer_root.join("pulled.txt"), "from remote pull\n")
            .expect("Git pull remote fixture should write");
        git_fixture_command(&peer_root, &["add", "pulled.txt"]);
        git_fixture_command(&peer_root, &["commit", "-m", "remote pull update"]);
        git_fixture_command(&peer_root, &["push", "origin", "main"]);

        (
            harness,
            workspace_id,
            workspace_root,
            session_id,
            remote_root,
            peer_parent,
            initial_head,
        )
    }

    fn prepare_git_merge_approval_case(
        label: &str,
        title: &str,
    ) -> (
        MagiTurnHarness,
        magi_core::WorkspaceId,
        PathBuf,
        SessionId,
        String,
    ) {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case(label, title);
        git_fixture_command(&workspace_root, &["switch", "approval-target"]);
        fs::write(workspace_root.join("merged.txt"), "from merge\n")
            .expect("Git merge fixture should write");
        git_fixture_command(&workspace_root, &["add", "merged.txt"]);
        git_fixture_command(&workspace_root, &["commit", "-m", "merge update"]);
        git_fixture_command(&workspace_root, &["switch", "main"]);
        let initial_head = current_git_head(&workspace_root);
        (
            harness,
            workspace_id,
            workspace_root,
            session_id,
            initial_head,
        )
    }

    async fn run_restricted_git_push_case(case_name: &'static str, decision: &'static str) {
        let title = format!("Git 推送审批验收-{case_name}");
        let (harness, workspace_id, workspace_root, session_id, remote_root) =
            prepare_git_push_approval_case(case_name, &title);
        harness.provider.set_tool_then_completed(
            "git_push",
            serde_json::json!({
                "remote": "origin",
                "branch": "main",
                "setUpstream": true,
            })
            .to_string(),
            format!("Git 推送审批 {case_name} 已收口"),
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                &format!("调用 git_push 推送 main 分支，等待审批后完成 {case_name} 验收"),
                &format!("harness-git-push-{case_name}-request"),
                &format!("harness-git-push-{case_name}-user"),
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 推送审批请求应被接纳");
        let turn_id = response.turn_id.clone().expect("Git 推送应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Git 推送应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "git_push");
        match decision {
            "allow_once" | "deny" => {
                resolve_tool_approval_via_http(
                    &harness,
                    &session_id,
                    &workspace_id,
                    &workspace_root,
                    &pending.approval_id,
                    decision,
                )
                .await;
            }
            "cancel" => {
                harness
                    .cancel_with_workspace(&session_id, Some(&workspace_id))
                    .await
                    .expect("取消 Git 推送审批应成功");
            }
            "expiry" => {
                assert_eq!(
                    harness
                        .state
                        .turn_coordinator()
                        .tool_approvals()
                        .expire_stale(
                            UtcMillis(UtcMillis::now().0 + TOOL_APPROVAL_TTL_MILLIS + 1,)
                        ),
                    1,
                    "测试必须显式推进 Git 推送审批时钟"
                );
            }
            other => panic!("未知 Git 推送审批决定: {other}"),
        }

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        let allowed = decision == "allow_once";
        assert_eq!(
            turn.status,
            // 拒绝或过期只结束这次调用，模型拿到结果后完成本轮（P1-9）。
            if decision == "cancel" {
                CanonicalTurnStatus::Cancelled
            } else {
                CanonicalTurnStatus::Completed
            }
        );
        assert_eq!(
            task.status,
            if decision == "cancel" {
                magi_core::TaskStatus::Killed
            } else {
                magi_core::TaskStatus::Completed
            }
        );
        assert_eq!(
            remote_git_branch_exists(remote_root.path(), "main"),
            allowed,
            "Git 推送审批结果必须与 bare remote 的真实 ref 副作用一致"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            if decision == "cancel" { 1 } else { 2 }
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            if matches!(decision, "allow_once" | "deny") {
                1
            } else {
                0
            }
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": format!("git_push_{case_name}"),
                "tool": "git_push",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": decision,
                "approval_requested": true,
                "approval_resolved": matches!(decision, "allow_once" | "deny"),
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": if decision == "cancel" {
                    "cancelled"
                } else {
                    "completed"
                },
                "task_status": if decision == "cancel" {
                    "killed"
                } else {
                    "completed"
                },
                "side_effect": if allowed { "remote_branch_pushed" } else { "remote_unchanged" },
            }),
            &turn,
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_push_allow_once_updates_real_remote() {
        run_restricted_git_push_case("allow_once", "allow_once").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_push_denial_preserves_remote() {
        run_restricted_git_push_case("deny", "deny").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_push_cancel_preserves_remote() {
        run_restricted_git_push_case("cancel", "cancel").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_push_expiry_preserves_remote() {
        run_restricted_git_push_case("expiry", "expiry").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_push_duplicate_replays_pending_approval() {
        let title = "Git 推送重复审批回放验收";
        let (harness, workspace_id, workspace_root, session_id, remote_root) =
            prepare_git_push_approval_case("duplicate", title);
        harness.provider.set_tool_then_completed(
            "git_push",
            serde_json::json!({
                "remote": "origin",
                "branch": "main",
                "setUpstream": true,
            })
            .to_string(),
            "Git 推送重复审批请求最终完成",
        );
        let request_id = "harness-git-push-duplicate-request";
        let user_message_id = "harness-git-push-duplicate-user";
        let prompt = "调用 git_push 推送 main 分支，等待审批后汇总重复请求结果";
        let first = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                prompt,
                request_id,
                user_message_id,
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 推送重复请求首次应被接纳");
        let first_turn_id = first.turn_id.clone().expect("首次 Git 推送应有 Turn");
        let first_task_id = first
            .root_task_id
            .clone()
            .expect("首次 Git 推送应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        let replay = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                prompt,
                request_id,
                user_message_id,
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 推送重复请求应回放原 Turn");
        assert_eq!(replay.turn_id.as_deref(), Some(first_turn_id.as_str()));
        assert_eq!(replay.root_task_id.as_deref(), Some(first_task_id.as_str()));
        assert_eq!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .len(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 1);
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
            &pending.approval_id,
            "allow_once",
        )
        .await;
        let turn = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(first_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert!(remote_git_branch_exists(remote_root.path(), "main"));
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            1
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "git_push_duplicate_pending",
                "tool": "git_push",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "duplicate_pending",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "remote_branch_pushed_once",
            }),
            &turn,
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    async fn run_restricted_git_pull_case(case_name: &'static str, decision: &'static str) {
        let title = format!("Git 拉取审批验收-{case_name}");
        let (
            harness,
            workspace_id,
            workspace_root,
            session_id,
            _remote_root,
            _peer_parent,
            initial_head,
        ) = prepare_git_pull_approval_case(case_name, &title);
        harness.provider.set_tool_then_completed(
            "git_pull",
            serde_json::json!({
                "remote": "origin",
                "branch": "main",
                "ffOnly": true,
            })
            .to_string(),
            format!("Git 拉取审批 {case_name} 已收口"),
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                &format!("调用 git_pull 拉取 origin/main，等待审批后完成 {case_name} 验收"),
                &format!("harness-git-pull-{case_name}-request"),
                &format!("harness-git-pull-{case_name}-user"),
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 拉取审批请求应被接纳");
        let turn_id = response.turn_id.clone().expect("Git 拉取应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Git 拉取应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "git_pull");
        match decision {
            "allow_once" | "deny" => {
                resolve_tool_approval_via_http(
                    &harness,
                    &session_id,
                    &workspace_id,
                    &workspace_root,
                    &pending.approval_id,
                    decision,
                )
                .await;
            }
            "cancel" => {
                harness
                    .cancel_with_workspace(&session_id, Some(&workspace_id))
                    .await
                    .expect("取消 Git 拉取审批应成功");
            }
            "expiry" => {
                assert_eq!(
                    harness
                        .state
                        .turn_coordinator()
                        .tool_approvals()
                        .expire_stale(
                            UtcMillis(UtcMillis::now().0 + TOOL_APPROVAL_TTL_MILLIS + 1,)
                        ),
                    1,
                    "测试必须显式推进 Git 拉取审批时钟"
                );
            }
            other => panic!("未知 Git 拉取审批决定: {other}"),
        }

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        let allowed = decision == "allow_once";
        assert_eq!(
            turn.status,
            // 拒绝或过期只结束这次调用，模型拿到结果后完成本轮（P1-9）。
            if decision == "cancel" {
                CanonicalTurnStatus::Cancelled
            } else {
                CanonicalTurnStatus::Completed
            }
        );
        assert_eq!(
            task.status,
            if decision == "cancel" {
                magi_core::TaskStatus::Killed
            } else {
                magi_core::TaskStatus::Completed
            }
        );
        assert_eq!(
            current_git_head(&workspace_root) != initial_head,
            allowed,
            "Git 拉取审批结果必须与本地 fast-forward 的真实副作用一致"
        );
        assert_eq!(
            fs::read_to_string(workspace_root.join("pulled.txt")).is_ok(),
            allowed,
            "Git 拉取审批结果必须与工作区文件副作用一致"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            if decision == "cancel" { 1 } else { 2 }
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            if matches!(decision, "allow_once" | "deny") {
                1
            } else {
                0
            }
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": format!("git_pull_{case_name}"),
                "tool": "git_pull",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": decision,
                "approval_requested": true,
                "approval_resolved": matches!(decision, "allow_once" | "deny"),
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": if decision == "cancel" {
                    "cancelled"
                } else {
                    "completed"
                },
                "task_status": if decision == "cancel" {
                    "killed"
                } else {
                    "completed"
                },
                "side_effect": if allowed {
                    "workspace_fast_forward_applied"
                } else {
                    "workspace_unchanged"
                },
            }),
            &turn,
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_pull_allow_once_updates_workspace_from_real_remote() {
        run_restricted_git_pull_case("allow_once", "allow_once").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_pull_denial_preserves_workspace() {
        run_restricted_git_pull_case("deny", "deny").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_pull_cancel_preserves_workspace() {
        run_restricted_git_pull_case("cancel", "cancel").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_pull_expiry_preserves_workspace() {
        run_restricted_git_pull_case("expiry", "expiry").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_pull_duplicate_replays_pending_approval() {
        let title = "Git 拉取重复审批回放验收";
        let (
            harness,
            workspace_id,
            workspace_root,
            session_id,
            _remote_root,
            _peer_parent,
            initial_head,
        ) = prepare_git_pull_approval_case("duplicate", title);
        harness.provider.set_tool_then_completed(
            "git_pull",
            serde_json::json!({
                "remote": "origin",
                "branch": "main",
                "ffOnly": true,
            })
            .to_string(),
            "Git 拉取重复审批请求最终完成",
        );
        let request_id = "harness-git-pull-duplicate-request";
        let user_message_id = "harness-git-pull-duplicate-user";
        let prompt = "调用 git_pull 拉取 origin/main，等待审批后汇总重复请求结果";
        let first = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                prompt,
                request_id,
                user_message_id,
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 拉取重复请求首次应被接纳");
        let first_turn_id = first.turn_id.clone().expect("首次 Git 拉取应有 Turn");
        let first_task_id = first
            .root_task_id
            .clone()
            .expect("首次 Git 拉取应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        let replay = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                prompt,
                request_id,
                user_message_id,
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 拉取重复请求应回放原 Turn");
        assert_eq!(replay.turn_id.as_deref(), Some(first_turn_id.as_str()));
        assert_eq!(replay.root_task_id.as_deref(), Some(first_task_id.as_str()));
        assert_eq!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .len(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 1);
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
            &pending.approval_id,
            "allow_once",
        )
        .await;
        let turn = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(first_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_ne!(current_git_head(&workspace_root), initial_head);
        assert_eq!(
            fs::read_to_string(workspace_root.join("pulled.txt"))
                .expect("Git 拉取重复放行后文件应存在"),
            "from remote pull\n"
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            1
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "git_pull_duplicate_pending",
                "tool": "git_pull",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "duplicate_pending",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "workspace_fast_forward_applied_once",
            }),
            &turn,
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    async fn run_restricted_git_merge_case(case_name: &'static str, decision: &'static str) {
        let title = format!("Git 合并审批验收-{case_name}");
        let (harness, workspace_id, workspace_root, session_id, initial_head) =
            prepare_git_merge_approval_case(case_name, &title);
        harness.provider.set_tool_then_completed(
            "git_merge",
            serde_json::json!({
                "target": "approval-target",
                "ffOnly": true,
                "confirm": true,
            })
            .to_string(),
            format!("Git 合并审批 {case_name} 已收口"),
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                &format!(
                    "先预览再调用 git_merge 合并 approval-target，等待审批后完成 {case_name} 验收"
                ),
                &format!("harness-git-merge-{case_name}-request"),
                &format!("harness-git-merge-{case_name}-user"),
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 合并审批请求应被接纳");
        let turn_id = response.turn_id.clone().expect("Git 合并应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Git 合并应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "git_merge");
        match decision {
            "allow_once" | "deny" => {
                resolve_tool_approval_via_http(
                    &harness,
                    &session_id,
                    &workspace_id,
                    &workspace_root,
                    &pending.approval_id,
                    decision,
                )
                .await;
            }
            "cancel" => {
                harness
                    .cancel_with_workspace(&session_id, Some(&workspace_id))
                    .await
                    .expect("取消 Git 合并审批应成功");
            }
            "expiry" => {
                assert_eq!(
                    harness
                        .state
                        .turn_coordinator()
                        .tool_approvals()
                        .expire_stale(
                            UtcMillis(UtcMillis::now().0 + TOOL_APPROVAL_TTL_MILLIS + 1,)
                        ),
                    1,
                    "测试必须显式推进 Git 合并审批时钟"
                );
            }
            other => panic!("未知 Git 合并审批决定: {other}"),
        }

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        let allowed = decision == "allow_once";
        assert_eq!(
            turn.status,
            // 拒绝或过期只结束这次调用，模型拿到结果后完成本轮（P1-9）。
            if decision == "cancel" {
                CanonicalTurnStatus::Cancelled
            } else {
                CanonicalTurnStatus::Completed
            }
        );
        assert_eq!(
            task.status,
            if decision == "cancel" {
                magi_core::TaskStatus::Killed
            } else {
                magi_core::TaskStatus::Completed
            }
        );
        assert_eq!(
            current_git_head(&workspace_root) != initial_head,
            allowed,
            "Git 合并审批结果必须与真实 fast-forward 副作用一致"
        );
        assert_eq!(
            fs::read_to_string(workspace_root.join("merged.txt")).is_ok(),
            allowed,
            "Git 合并审批结果必须与工作区文件副作用一致"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            if decision == "cancel" { 1 } else { 2 }
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            if matches!(decision, "allow_once" | "deny") {
                1
            } else {
                0
            }
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": format!("git_merge_{case_name}"),
                "tool": "git_merge",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": decision,
                "approval_requested": true,
                "approval_resolved": matches!(decision, "allow_once" | "deny"),
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": if decision == "cancel" {
                    "cancelled"
                } else {
                    "completed"
                },
                "task_status": if decision == "cancel" {
                    "killed"
                } else {
                    "completed"
                },
                "side_effect": if allowed {
                    "workspace_fast_forward_merge_applied"
                } else {
                    "workspace_unchanged"
                },
            }),
            &turn,
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_merge_allow_once_updates_workspace() {
        run_restricted_git_merge_case("allow_once", "allow_once").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_merge_denial_preserves_workspace() {
        run_restricted_git_merge_case("deny", "deny").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_merge_cancel_preserves_workspace() {
        run_restricted_git_merge_case("cancel", "cancel").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_merge_expiry_preserves_workspace() {
        run_restricted_git_merge_case("expiry", "expiry").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_merge_duplicate_replays_pending_approval() {
        let title = "Git 合并重复审批回放验收";
        let (harness, workspace_id, workspace_root, session_id, initial_head) =
            prepare_git_merge_approval_case("duplicate", title);
        harness.provider.set_tool_then_completed(
            "git_merge",
            serde_json::json!({
                "target": "approval-target",
                "ffOnly": true,
                "confirm": true,
            })
            .to_string(),
            "Git 合并重复审批请求最终完成",
        );
        let request_id = "harness-git-merge-duplicate-request";
        let user_message_id = "harness-git-merge-duplicate-user";
        let prompt = "先预览再调用 git_merge 合并 approval-target，等待审批后汇总重复请求结果";
        let first = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                prompt,
                request_id,
                user_message_id,
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 合并重复请求首次应被接纳");
        let first_turn_id = first.turn_id.clone().expect("首次 Git 合并应有 Turn");
        let first_task_id = first
            .root_task_id
            .clone()
            .expect("首次 Git 合并应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        let replay = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                prompt,
                request_id,
                user_message_id,
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 合并重复请求应回放原 Turn");
        assert_eq!(replay.turn_id.as_deref(), Some(first_turn_id.as_str()));
        assert_eq!(replay.root_task_id.as_deref(), Some(first_task_id.as_str()));
        assert_eq!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .len(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 1);
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
            &pending.approval_id,
            "allow_once",
        )
        .await;
        let turn = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(first_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_ne!(current_git_head(&workspace_root), initial_head);
        assert_eq!(
            fs::read_to_string(workspace_root.join("merged.txt"))
                .expect("Git 合并重复放行后文件应存在"),
            "from merge\n"
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            1
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "git_merge_duplicate_pending",
                "tool": "git_merge",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "duplicate_pending",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "workspace_fast_forward_merge_applied_once",
            }),
            &turn,
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    async fn run_restricted_git_worktree_create_case(
        case_name: &'static str,
        decision: &'static str,
    ) {
        let title = format!("Git worktree 创建审批验收-{case_name}");
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case(case_name, &title);
        let initial_worktree_count = git_worktree_count(&workspace_root);
        harness.provider.set_tool_then_completed(
            "git_worktree_create",
            serde_json::json!({
                "mode": "writable",
                "branch": "approval-worktree",
                "allocationKey": "approval-worktree",
            })
            .to_string(),
            format!("Git worktree 创建审批 {case_name} 已收口"),
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                &format!("调用 git_worktree_create 创建 writable worktree，等待审批后完成 {case_name} 验收"),
                &format!("harness-git-worktree-create-{case_name}-request"),
                &format!("harness-git-worktree-create-{case_name}-user"),
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git worktree 创建审批请求应被接纳");
        let turn_id = response
            .turn_id
            .clone()
            .expect("Git worktree 创建应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Git worktree 创建应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "git_worktree_create");
        match decision {
            "allow_once" | "deny" => {
                resolve_tool_approval_via_http(
                    &harness,
                    &session_id,
                    &workspace_id,
                    &workspace_root,
                    &pending.approval_id,
                    decision,
                )
                .await;
            }
            "cancel" => {
                harness
                    .cancel_with_workspace(&session_id, Some(&workspace_id))
                    .await
                    .expect("取消 Git worktree 创建审批应成功");
            }
            "expiry" => {
                assert_eq!(
                    harness
                        .state
                        .turn_coordinator()
                        .tool_approvals()
                        .expire_stale(
                            UtcMillis(UtcMillis::now().0 + TOOL_APPROVAL_TTL_MILLIS + 1,)
                        ),
                    1,
                    "测试必须显式推进 Git worktree 创建审批时钟"
                );
            }
            other => panic!("未知 Git worktree 创建审批决定: {other}"),
        }

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        let allowed = decision == "allow_once";
        assert_eq!(
            turn.status,
            // 拒绝或过期只结束这次调用，模型拿到结果后完成本轮（P1-9）。
            if decision == "cancel" {
                CanonicalTurnStatus::Cancelled
            } else {
                CanonicalTurnStatus::Completed
            }
        );
        assert_eq!(
            task.status,
            if decision == "cancel" {
                magi_core::TaskStatus::Killed
            } else {
                magi_core::TaskStatus::Completed
            }
        );
        let worktree_paths = git_worktree_paths(&workspace_root);
        assert_eq!(
            worktree_paths.len() > initial_worktree_count,
            allowed,
            "Git worktree 审批结果必须与真实 worktree 副作用一致"
        );
        if allowed {
            assert!(
                worktree_paths
                    .iter()
                    .any(|path| !same_git_path(path, &workspace_root)),
                "放行后必须出现额外的 Magi 管理 worktree"
            );
        }
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            if decision == "cancel" { 1 } else { 2 }
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            if matches!(decision, "allow_once" | "deny") {
                1
            } else {
                0
            }
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": format!("git_worktree_create_{case_name}"),
                "tool": "git_worktree_create",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": decision,
                "approval_requested": true,
                "approval_resolved": matches!(decision, "allow_once" | "deny"),
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": if decision == "cancel" {
                    "cancelled"
                } else {
                    "completed"
                },
                "task_status": if decision == "cancel" {
                    "killed"
                } else {
                    "completed"
                },
                "side_effect": if allowed {
                    "managed_worktree_created"
                } else {
                    "worktree_unchanged"
                },
            }),
            &turn,
        );
        for path in git_worktree_paths(&workspace_root)
            .into_iter()
            .filter(|path| !same_git_path(path, &workspace_root))
        {
            let path_string = path.to_string_lossy().to_string();
            git_fixture_command(
                &workspace_root,
                &["worktree", "remove", "--force", path_string.as_str()],
            );
        }
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_worktree_create_allow_once_creates_real_worktree() {
        run_restricted_git_worktree_create_case("allow_once", "allow_once").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_worktree_create_denial_preserves_worktrees() {
        run_restricted_git_worktree_create_case("deny", "deny").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_worktree_create_cancel_preserves_worktrees() {
        run_restricted_git_worktree_create_case("cancel", "cancel").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_worktree_create_expiry_preserves_worktrees() {
        run_restricted_git_worktree_create_case("expiry", "expiry").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_worktree_create_duplicate_replays_pending_approval() {
        let title = "Git worktree 创建重复审批回放验收";
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("duplicate", title);
        let initial_worktree_count = git_worktree_count(&workspace_root);
        harness.provider.set_tool_then_completed(
            "git_worktree_create",
            serde_json::json!({
                "mode": "writable",
                "branch": "approval-worktree",
                "allocationKey": "approval-worktree",
            })
            .to_string(),
            "Git worktree 创建重复审批请求最终完成",
        );
        let request_id = "harness-git-worktree-create-duplicate-request";
        let user_message_id = "harness-git-worktree-create-duplicate-user";
        let prompt = "调用 git_worktree_create 创建 writable worktree，等待审批后汇总重复请求结果";
        let first = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                prompt,
                request_id,
                user_message_id,
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git worktree 创建重复请求首次应被接纳");
        let first_turn_id = first
            .turn_id
            .clone()
            .expect("首次 Git worktree 创建应有 Turn");
        let first_task_id = first
            .root_task_id
            .clone()
            .expect("首次 Git worktree 创建应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        let replay = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                prompt,
                request_id,
                user_message_id,
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git worktree 创建重复请求应回放原 Turn");
        assert_eq!(replay.turn_id.as_deref(), Some(first_turn_id.as_str()));
        assert_eq!(replay.root_task_id.as_deref(), Some(first_task_id.as_str()));
        assert_eq!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .len(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 1);
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
            &pending.approval_id,
            "allow_once",
        )
        .await;
        let turn = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(first_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            git_worktree_count(&workspace_root),
            initial_worktree_count + 1,
            "重复放行只能创建一个 worktree"
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            1
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "git_worktree_create_duplicate_pending",
                "tool": "git_worktree_create",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "duplicate_pending",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "managed_worktree_created_once",
            }),
            &turn,
        );
        for path in git_worktree_paths(&workspace_root)
            .into_iter()
            .filter(|path| !same_git_path(path, &workspace_root))
        {
            let path_string = path.to_string_lossy().to_string();
            git_fixture_command(
                &workspace_root,
                &["worktree", "remove", "--force", path_string.as_str()],
            );
        }
        let _ = fs::remove_dir_all(workspace_root);
    }

    fn prepare_git_worktree_remove_approval_case(
        label: &str,
        title: &str,
    ) -> (
        MagiTurnHarness,
        magi_core::WorkspaceId,
        PathBuf,
        SessionId,
        PathBuf,
        usize,
    ) {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case(label, title);
        let state_root = harness
            .state
            .runtime_persistence()
            .and_then(RuntimeStatePersistence::state_root)
            .expect("Git worktree remove harness should expose runtime state root");
        let managed_root = state_root.join("worktrees").join(workspace_id.to_string());
        fs::create_dir_all(&managed_root).expect("Git worktree remove managed root should create");
        let remove_path = managed_root.join("approval-remove");
        let remove_path_string = remove_path.to_string_lossy().to_string();
        git_fixture_command(
            &workspace_root,
            &[
                "worktree",
                "add",
                "-b",
                "approval-remove",
                remove_path_string.as_str(),
                "HEAD",
            ],
        );
        let initial_worktree_count = git_worktree_count(&workspace_root);
        (
            harness,
            workspace_id,
            workspace_root,
            session_id,
            remove_path,
            initial_worktree_count,
        )
    }

    async fn run_restricted_git_worktree_remove_case(
        case_name: &'static str,
        decision: &'static str,
    ) {
        let title = format!("Git worktree 移除审批验收-{case_name}");
        let (
            harness,
            workspace_id,
            workspace_root,
            session_id,
            remove_path,
            initial_worktree_count,
        ) = prepare_git_worktree_remove_approval_case(case_name, &title);
        let remove_path_string = remove_path.to_string_lossy().to_string();
        harness.provider.set_tool_then_completed(
            "git_worktree_remove",
            serde_json::json!({
                "path": remove_path_string,
                "force": false,
            })
            .to_string(),
            format!("Git worktree 移除审批 {case_name} 已收口"),
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                &format!(
                    "调用 git_worktree_remove 移除管理 worktree，等待审批后完成 {case_name} 验收"
                ),
                &format!("harness-git-worktree-remove-{case_name}-request"),
                &format!("harness-git-worktree-remove-{case_name}-user"),
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git worktree 移除审批请求应被接纳");
        let turn_id = response
            .turn_id
            .clone()
            .expect("Git worktree 移除应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Git worktree 移除应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "git_worktree_remove");
        match decision {
            "allow_once" | "deny" => {
                resolve_tool_approval_via_http(
                    &harness,
                    &session_id,
                    &workspace_id,
                    &workspace_root,
                    &pending.approval_id,
                    decision,
                )
                .await;
            }
            "cancel" => {
                harness
                    .cancel_with_workspace(&session_id, Some(&workspace_id))
                    .await
                    .expect("取消 Git worktree 移除审批应成功");
            }
            "expiry" => {
                assert_eq!(
                    harness
                        .state
                        .turn_coordinator()
                        .tool_approvals()
                        .expire_stale(
                            UtcMillis(UtcMillis::now().0 + TOOL_APPROVAL_TTL_MILLIS + 1,)
                        ),
                    1,
                    "测试必须显式推进 Git worktree 移除审批时钟"
                );
            }
            other => panic!("未知 Git worktree 移除审批决定: {other}"),
        }

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        let allowed = decision == "allow_once";
        assert_eq!(
            turn.status,
            // 拒绝或过期只结束这次调用，模型拿到结果后完成本轮（P1-9）。
            if decision == "cancel" {
                CanonicalTurnStatus::Cancelled
            } else {
                CanonicalTurnStatus::Completed
            }
        );
        assert_eq!(
            task.status,
            if decision == "cancel" {
                magi_core::TaskStatus::Killed
            } else {
                magi_core::TaskStatus::Completed
            }
        );
        assert_eq!(
            git_worktree_count(&workspace_root),
            if allowed {
                initial_worktree_count - 1
            } else {
                initial_worktree_count
            },
            "Git worktree 移除审批结果必须与真实 worktree 副作用一致"
        );
        assert_eq!(remove_path.exists(), !allowed);
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            if decision == "cancel" { 1 } else { 2 }
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            if matches!(decision, "allow_once" | "deny") {
                1
            } else {
                0
            }
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": format!("git_worktree_remove_{case_name}"),
                "tool": "git_worktree_remove",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": decision,
                "approval_requested": true,
                "approval_resolved": matches!(decision, "allow_once" | "deny"),
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": if decision == "cancel" {
                    "cancelled"
                } else {
                    "completed"
                },
                "task_status": if decision == "cancel" {
                    "killed"
                } else {
                    "completed"
                },
                "side_effect": if allowed {
                    "managed_worktree_removed"
                } else {
                    "worktree_unchanged"
                },
            }),
            &turn,
        );
        if remove_path.exists() {
            let remove_path_string = remove_path.to_string_lossy().to_string();
            git_fixture_command(
                &workspace_root,
                &["worktree", "remove", "--force", remove_path_string.as_str()],
            );
        }
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_worktree_remove_allow_once_removes_real_worktree() {
        run_restricted_git_worktree_remove_case("allow_once", "allow_once").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_worktree_remove_denial_preserves_worktree() {
        run_restricted_git_worktree_remove_case("deny", "deny").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_worktree_remove_cancel_preserves_worktree() {
        run_restricted_git_worktree_remove_case("cancel", "cancel").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_worktree_remove_expiry_preserves_worktree() {
        run_restricted_git_worktree_remove_case("expiry", "expiry").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_worktree_remove_duplicate_replays_pending_approval() {
        let title = "Git worktree 移除重复审批回放验收";
        let (
            harness,
            workspace_id,
            workspace_root,
            session_id,
            remove_path,
            initial_worktree_count,
        ) = prepare_git_worktree_remove_approval_case("duplicate", title);
        let remove_path_string = remove_path.to_string_lossy().to_string();
        harness.provider.set_tool_then_completed(
            "git_worktree_remove",
            serde_json::json!({
                "path": remove_path_string,
                "force": false,
            })
            .to_string(),
            "Git worktree 移除重复审批请求最终完成",
        );
        let request_id = "harness-git-worktree-remove-duplicate-request";
        let user_message_id = "harness-git-worktree-remove-duplicate-user";
        let prompt = "调用 git_worktree_remove 移除管理 worktree，等待审批后汇总重复请求结果";
        let first = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                prompt,
                request_id,
                user_message_id,
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git worktree 移除重复请求首次应被接纳");
        let first_turn_id = first
            .turn_id
            .clone()
            .expect("首次 Git worktree 移除应有 Turn");
        let first_task_id = first
            .root_task_id
            .clone()
            .expect("首次 Git worktree 移除应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        let replay = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                prompt,
                request_id,
                user_message_id,
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git worktree 移除重复请求应回放原 Turn");
        assert_eq!(replay.turn_id.as_deref(), Some(first_turn_id.as_str()));
        assert_eq!(replay.root_task_id.as_deref(), Some(first_task_id.as_str()));
        assert_eq!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .len(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 1);
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
            &pending.approval_id,
            "allow_once",
        )
        .await;
        let turn = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(first_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            git_worktree_count(&workspace_root),
            initial_worktree_count - 1
        );
        assert!(!remove_path.exists());
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            1
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "git_worktree_remove_duplicate_pending",
                "tool": "git_worktree_remove",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "duplicate_pending",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "managed_worktree_removed_once",
            }),
            &turn,
        );
        if remove_path.exists() {
            let remove_path_string = remove_path.to_string_lossy().to_string();
            git_fixture_command(
                &workspace_root,
                &["worktree", "remove", "--force", remove_path_string.as_str()],
            );
        }
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[allow(clippy::too_many_arguments)]
    fn record_git_approval_matrix_row(
        turn: &CanonicalTurn,
        case_name: &str,
        lifecycle: &str,
        branch_before: &str,
        branch_after: &str,
        approval_requested: bool,
        approval_resolved: bool,
        provider_requests: usize,
        turn_status: &str,
        task_status: &str,
        side_effect: &str,
    ) {
        static ROWS: OnceLock<Mutex<Vec<serde_json::Value>>> = OnceLock::new();
        let rows = ROWS.get_or_init(|| Mutex::new(Vec::new()));
        let mut rows = rows.lock().expect("Git matrix rows lock should hold");
        rows.retain(|row| row["case"] != case_name);
        let row = serde_json::json!({
            "case": case_name,
            "tool": "git_branch_switch",
            "surface": "git_workspace",
            "access_profile": "Restricted",
            "scope": "workspace_internal",
            "lifecycle": lifecycle,
            "branch_before": branch_before,
            "branch_after": branch_after,
            "approval_requested": approval_requested,
            "approval_resolved": approval_resolved,
            "provider_requests": provider_requests,
            "turn_status": turn_status,
            "task_status": task_status,
            "side_effect": side_effect,
        });
        rows.push(row.clone());
        rows.sort_by(|left, right| left["case"].as_str().cmp(&right["case"].as_str()));
        record_turn_permission_matrix_row(row, turn);
        let path = std::env::var_os("MAGI_API_GIT_APPROVAL_MATRIX_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp/magi-api-git-approval-matrix.json"));
        fs::write(
            path,
            serde_json::to_vec_pretty(&*rows).expect("Git matrix rows should serialize"),
        )
        .expect("Git matrix evidence should write");
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_switch_allow_once_changes_real_branch() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("allow", "Git 分支审批允许验收");
        harness.provider.set_tool_then_completed(
            "git_branch_switch",
            serde_json::json!({"branch": "approval-target"}).to_string(),
            "Git 分支已在审批后切换",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "调用 git_branch_switch 切换到 approval-target，等待审批后汇总结果",
                "harness-git-approval-allow-request",
                "harness-git-approval-allow-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 分支审批请求应被接纳");
        let turn_id = response.turn_id.clone().expect("Git 分支审批请求应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Git 分支审批请求应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "git_branch_switch");
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
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
            current_git_branch(&workspace_root),
            "approval-target",
            "allow_once 后必须产生真实 Git 分支副作用"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "Git 审批放行后只允许原始工具轮和一次最终答复轮"
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1,
            "Git 分支切换只应请求一次审批"
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            1,
            "Git 分支审批允许后应收口一次"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "git_branch_switch"
                        && tool.result.is_some()
                        && tool.error.is_none()
                })
        }));

        record_git_approval_matrix_row(
            &turn,
            "allow_once",
            "allow_once",
            "main",
            &current_git_branch(&workspace_root),
            true,
            true,
            non_classifier_provider_request_count(&harness),
            "completed",
            "completed",
            "branch_switched",
        );

        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn full_access_profile_git_branch_switch_runs_without_approval() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("full-access-switch", "FullAccess Git 分支直接执行验收");
        harness.provider.set_tool_then_completed(
            "git_branch_switch",
            serde_json::json!({"branch": "approval-target"}).to_string(),
            "FullAccess Git 分支已切换",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "调用 git_branch_switch 切换到 approval-target，无需审批并汇总结果",
                "harness-git-full-access-switch-request",
                "harness-git-full-access-switch-user",
                Some(AccessProfile::FullAccess),
            )
            .await
            .expect("FullAccess Git branch switch should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("FullAccess Git branch switch should have Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("FullAccess Git branch switch should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;

        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(current_git_branch(&workspace_root), "approval-target");
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested")
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "git_branch_switch_full_access",
                "tool": "git_branch_switch",
                "surface": "git_workspace",
                "access_profile": "FullAccess",
                "scope": "workspace_internal",
                "lifecycle": "allow",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "branch_switched",
            }),
            &turn,
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn full_access_profile_git_branch_create_runs_without_approval() {
        let harness = MagiTurnHarness::new_task("完全授权 Git 创建分支");
        let (workspace_id, workspace_root) = register_git_workspace(&harness);
        let session_id = SessionId::new("harness-full-access-git-create-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "完全授权 Git 创建分支验收",
                Some(workspace_id.to_string()),
            )
            .expect("FullAccess Git session should create");
        harness.provider.set_tool_then_completed(
            "git_branch_create",
            serde_json::json!({
                "branch": "full-access-created",
                "startPoint": "main",
                "switch": false,
            })
            .to_string(),
            "完全授权 Git 分支创建完成",
        );
        let request_id = "harness-full-access-git-create-request";
        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "执行一个任务：调用 git_branch_create 创建 full-access-created 分支",
                request_id,
                "harness-full-access-git-create-user",
                Some(AccessProfile::FullAccess),
            )
            .await
            .expect("FullAccess Git request should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("FullAccess Git Turn should exist");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("FullAccess Git root task should exist");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert!(git_branch_exists(&workspace_root, "full-access-created"));
        assert_eq!(current_git_branch(&workspace_root), "main");
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "FullAccess Git mutation must not request approval"
        );
        record_unified_permission_matrix_row(serde_json::json!({
            "case": "git_branch_create_full_access",
            "tool": "git_branch_create",
            "surface": "git_workspace",
            "access_profile": "FullAccess",
            "scope": "workspace_internal",
            "lifecycle": "allow",
            "approval_requested": false,
            "approval_resolved": false,
            "provider_requests": non_classifier_provider_request_count(&harness),
            "turn_status": "completed",
            "task_status": "completed",
            "side_effect": "branch_created",
            "request_id": request_id,
            "turn_id": turn_id,
            "execution_profile": "task",
        }));
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_create_allow_once_creates_real_branch() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("create-allow", "Git 创建分支审批允许验收");
        harness.provider.set_tool_then_completed(
            "git_branch_create",
            serde_json::json!({
                "branch": "approval-created",
                "startPoint": "main",
                "switch": false,
            })
            .to_string(),
            "Git 分支已在审批后创建",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "调用 git_branch_create 创建 approval-created 分支，等待审批后汇总结果",
                "harness-git-create-allow-request",
                "harness-git-create-allow-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 创建分支审批请求应被接纳");
        let turn_id = response.turn_id.clone().expect("Git 创建分支应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Git 创建分支应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "git_branch_create");
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
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
        assert!(
            git_branch_exists(&workspace_root, "approval-created"),
            "allow_once 后必须产生真实 Git 分支创建副作用"
        );
        assert_eq!(current_git_branch(&workspace_root), "main");
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            1
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "git_branch_create_allow_once",
                "tool": "git_branch_create",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "allow_once",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "branch_created",
            }),
            &turn,
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_create_denial_preserves_branch_state() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("create-deny", "Git 创建分支审批拒绝验收");
        harness.provider.set_tool_then_completed(
            "git_branch_create",
            serde_json::json!({
                "branch": "approval-denied",
                "startPoint": "main",
                "switch": false,
            })
            .to_string(),
            "Git 创建分支被拒绝",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "调用 git_branch_create 创建 approval-denied 分支，但必须等待审批",
                "harness-git-create-deny-request",
                "harness-git-create-deny-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 创建分支拒绝请求应被接纳");
        let turn_id = response.turn_id.clone().expect("Git 创建分支拒绝应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Git 创建分支拒绝应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "git_branch_create");
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
            &pending.approval_id,
            "deny",
        )
        .await;

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        // 拒绝或过期只结束这次调用，模型拿到结果后完成本轮（P1-9）。
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert!(
            !git_branch_exists(&workspace_root, "approval-denied"),
            "拒绝 Git 分支创建后不得产生真实分支副作用"
        );
        assert_eq!(current_git_branch(&workspace_root), "main");
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            1
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "git_branch_create_deny",
                "tool": "git_branch_create",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "deny",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "branch_unchanged",
            }),
            &turn,
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    async fn run_restricted_git_branch_create_terminal_case(
        case_name: &'static str,
        lifecycle: &'static str,
    ) {
        let title = format!("Git 创建分支审批终态验收-{case_name}");
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case(case_name, &title);
        let branch = format!("approval-created-{case_name}");
        harness.provider.set_tool_then_completed(
            "git_branch_create",
            serde_json::json!({
                "branch": branch,
                "startPoint": "main",
                "switch": false,
            })
            .to_string(),
            format!("Git 创建分支审批 {case_name} 已收口"),
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                &format!("调用 git_branch_create 创建 {branch}，等待审批后完成 {case_name} 验收"),
                &format!("harness-git-create-{case_name}-request"),
                &format!("harness-git-create-{case_name}-user"),
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 创建分支终态请求应被接纳");
        let turn_id = response.turn_id.clone().expect("Git 创建分支应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Git 创建分支应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "git_branch_create");
        match lifecycle {
            "cancel" => {
                harness
                    .cancel_with_workspace(&session_id, Some(&workspace_id))
                    .await
                    .expect("取消 Git 创建分支审批应成功");
            }
            "expiry" => {
                assert_eq!(
                    harness
                        .state
                        .turn_coordinator()
                        .tool_approvals()
                        .expire_stale(
                            UtcMillis(UtcMillis::now().0 + TOOL_APPROVAL_TTL_MILLIS + 1,)
                        ),
                    1,
                    "测试必须显式推进 Git 创建分支审批时钟"
                );
            }
            other => panic!("未知 Git 创建分支终态: {other}"),
        }

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(
            turn.status,
            // 过期只结束这次调用，模型拿到结果后完成本轮（P1-9）。
            if lifecycle == "cancel" {
                CanonicalTurnStatus::Cancelled
            } else {
                CanonicalTurnStatus::Completed
            }
        );
        if lifecycle == "cancel" {
            assert!(matches!(
                task.status,
                magi_core::TaskStatus::Failed | magi_core::TaskStatus::Killed
            ));
        } else {
            assert_eq!(task.status, magi_core::TaskStatus::Completed);
        }
        assert!(
            !git_branch_exists(&workspace_root, &branch),
            "Git 创建分支未完成审批时不得产生真实分支副作用"
        );
        assert_eq!(current_git_branch(&workspace_root), "main");
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            if lifecycle == "cancel" { 1 } else { 2 }
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            0,
            "取消或过期不得伪造 resolved 事件"
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": format!("git_branch_create_{case_name}"),
                "tool": "git_branch_create",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": lifecycle,
                "approval_requested": true,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": if lifecycle == "cancel" { "cancelled" } else { "completed" },
                "task_status": if lifecycle == "cancel" { "failed_or_killed" } else { "completed" },
                "side_effect": "branch_unchanged",
            }),
            &turn,
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_create_cancel_preserves_branch_state() {
        run_restricted_git_branch_create_terminal_case("cancel", "cancel").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_create_expiry_preserves_branch_state() {
        run_restricted_git_branch_create_terminal_case("expiry", "expiry").await;
    }

    async fn run_restricted_git_branch_duplicate_pending_case(
        tool_name: &'static str,
        case_name: &'static str,
        branch: &'static str,
        arguments: serde_json::Value,
        expected_side_effect: &'static str,
        branch_should_exist_after: bool,
    ) {
        let title = format!("Git {tool_name} 重复审批回放验收");
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case(case_name, &title);
        harness.provider.set_tool_then_completed(
            tool_name,
            arguments.to_string(),
            format!("Git {tool_name} 重复审批请求最终完成"),
        );
        let request_id = format!("harness-git-{tool_name}-{case_name}-request");
        let user_message_id = format!("harness-git-{tool_name}-{case_name}-user");
        let prompt = format!("调用 {tool_name} 操作 {branch}，等待审批后汇总重复请求结果");

        let first = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                &prompt,
                &request_id,
                &user_message_id,
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 重复审批首次请求应被接纳");
        let first_turn_id = first.turn_id.clone().expect("Git 重复审批应有 Turn");
        let first_task_id = first
            .root_task_id
            .clone()
            .expect("Git 重复审批应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, tool_name);

        let replay = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                &prompt,
                &request_id,
                &user_message_id,
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 重复审批请求应回放原 Turn");
        assert_eq!(replay.turn_id.as_deref(), Some(first_turn_id.as_str()));
        assert_eq!(replay.root_task_id.as_deref(), Some(first_task_id.as_str()));
        assert_eq!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .len(),
            1,
            "重复 Git 请求不得创建第二个 pending 审批"
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1,
            "重复 Git 请求不得重复发布审批事件"
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 1);

        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
            &pending.approval_id,
            "allow_once",
        )
        .await;
        let turn = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(first_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            git_branch_exists(&workspace_root, branch),
            branch_should_exist_after,
            "重复 Git 审批放行后的真实分支副作用必须只执行一次"
        );
        assert_eq!(current_git_branch(&workspace_root), "main");
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            1
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": format!("{tool_name}_duplicate_pending"),
                "tool": tool_name,
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "duplicate_pending",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": expected_side_effect,
            }),
            &turn,
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_create_duplicate_replays_pending_approval() {
        run_restricted_git_branch_duplicate_pending_case(
            "git_branch_create",
            "create-duplicate",
            "approval-created-duplicate",
            serde_json::json!({
                "branch": "approval-created-duplicate",
                "startPoint": "main",
                "switch": false,
            }),
            "branch_created_once",
            true,
        )
        .await;
    }

    async fn run_restricted_git_branch_delete_case(
        case_name: &'static str,
        decision: &'static str,
    ) {
        let title = format!("Git 删除分支审批验收-{case_name}");
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case(case_name, &title);
        harness.provider.set_tool_then_completed(
            "git_branch_delete",
            serde_json::json!({
                "branch": "approval-target",
                "force": false,
            })
            .to_string(),
            format!("Git 删除分支审批 {case_name} 已收口"),
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                &format!(
                    "调用 git_branch_delete 删除 approval-target 分支，等待审批后完成 {case_name} 验收"
                ),
                &format!("harness-git-delete-{case_name}-request"),
                &format!("harness-git-delete-{case_name}-user"),
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 删除分支审批请求应被接纳");
        let turn_id = response.turn_id.clone().expect("Git 删除分支应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("Git 删除分支应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "git_branch_delete");
        match decision {
            "allow_once" | "deny" => {
                resolve_tool_approval_via_http(
                    &harness,
                    &session_id,
                    &workspace_id,
                    &workspace_root,
                    &pending.approval_id,
                    decision,
                )
                .await;
            }
            "cancel" => {
                harness
                    .cancel_with_workspace(&session_id, Some(&workspace_id))
                    .await
                    .expect("取消 Git 删除分支审批应成功");
            }
            "expiry" => {
                assert_eq!(
                    harness
                        .state
                        .turn_coordinator()
                        .tool_approvals()
                        .expire_stale(
                            UtcMillis(UtcMillis::now().0 + TOOL_APPROVAL_TTL_MILLIS + 1,)
                        ),
                    1,
                    "测试必须显式推进 Git 删除分支审批时钟"
                );
            }
            other => panic!("未知 Git 删除分支审批决定: {other}"),
        }

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        let allowed = decision == "allow_once";
        assert_eq!(
            turn.status,
            // 拒绝或过期只结束这次调用，模型拿到结果后完成本轮（P1-9）。
            if decision == "cancel" {
                CanonicalTurnStatus::Cancelled
            } else {
                CanonicalTurnStatus::Completed
            }
        );
        assert_eq!(
            task.status,
            if decision == "cancel" {
                magi_core::TaskStatus::Killed
            } else {
                magi_core::TaskStatus::Completed
            }
        );
        assert_eq!(
            git_branch_exists(&workspace_root, "approval-target"),
            !allowed,
            "Git 删除分支的审批结果必须与真实分支副作用一致"
        );
        assert_eq!(current_git_branch(&workspace_root), "main");
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            if decision == "cancel" { 1 } else { 2 }
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            if matches!(decision, "allow_once" | "deny") {
                1
            } else {
                0
            }
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": format!("git_branch_delete_{case_name}"),
                "tool": "git_branch_delete",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": decision,
                "approval_requested": true,
                "approval_resolved": matches!(decision, "allow_once" | "deny"),
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": if decision == "cancel" {
                    "cancelled"
                } else {
                    "completed"
                },
                "task_status": if decision == "cancel" { "killed" } else { "completed" },
                "side_effect": if allowed { "branch_deleted" } else { "branch_unchanged" },
            }),
            &turn,
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_delete_allow_once_deletes_real_branch() {
        run_restricted_git_branch_delete_case("allow_once", "allow_once").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_delete_denial_preserves_branch() {
        run_restricted_git_branch_delete_case("deny", "deny").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_delete_cancel_preserves_branch() {
        run_restricted_git_branch_delete_case("cancel", "cancel").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_delete_expiry_preserves_branch() {
        run_restricted_git_branch_delete_case("expiry", "expiry").await;
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_delete_duplicate_replays_pending_approval() {
        run_restricted_git_branch_duplicate_pending_case(
            "git_branch_delete",
            "delete-duplicate",
            "approval-target",
            serde_json::json!({
                "branch": "approval-target",
                "force": false,
            }),
            "branch_deleted_once",
            false,
        )
        .await;
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_switch_duplicate_replays_pending_approval() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("duplicate", "Git 分支重复审批回放验收");
        harness.provider.set_tool_then_completed(
            "git_branch_switch",
            serde_json::json!({"branch": "approval-target"}).to_string(),
            "Git 分支重复请求最终完成",
        );

        let first = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "调用 git_branch_switch 切换到 approval-target，等待审批后汇总结果",
                "harness-git-approval-duplicate-request",
                "harness-git-approval-duplicate-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 重复审批首次请求应被接纳");
        let first_turn_id = first.turn_id.clone().expect("首次请求应有 Turn");
        let first_root_task_id = first.root_task_id.clone().expect("首次请求应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;

        let replay = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "调用 git_branch_switch 切换到 approval-target，等待审批后汇总结果",
                "harness-git-approval-duplicate-request",
                "harness-git-approval-duplicate-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 重复审批请求应回放原 Turn");
        assert_eq!(replay.turn_id.as_deref(), Some(first_turn_id.as_str()));
        assert_eq!(
            replay.root_task_id.as_deref(),
            Some(first_root_task_id.as_str())
        );
        assert_eq!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .len(),
            1,
            "重复 Git 请求不得创建第二个 pending 审批"
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 1);

        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
            &pending.approval_id,
            "allow_once",
        )
        .await;
        let turn = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(
            current_git_branch(&workspace_root),
            "approval-target",
            "重复提交回放后仍只应产生一次真实分支切换"
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            1
        );
        record_git_approval_matrix_row(
            &turn,
            "duplicate",
            "duplicate_pending",
            "main",
            &current_git_branch(&workspace_root),
            true,
            true,
            non_classifier_provider_request_count(&harness),
            "completed",
            "completed",
            "branch_switched_once",
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    async fn wait_for_pending_user_question(
        harness: &MagiTurnHarness,
        session_id: &SessionId,
    ) -> magi_conversation_runtime::PendingUserQuestion {
        for _ in 0..300 {
            if let Some(pending) = harness
                .state
                .conversation_registry
                .user_questions()
                .pending_for_session(session_id)
                .into_iter()
                .next()
                && harness
                    .events_for(session_id)
                    .iter()
                    .any(|event| event.event_type == "user.question.requested")
            {
                return pending;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("Turn {session_id} 未在测试窗口内产生向用户的提问");
    }

    fn ask_user_question_arguments() -> String {
        serde_json::json!({"questions": [{
            "question": "用哪种数据库？",
            "header": "数据库",
            "multiSelect": false,
            "options": [
                {"label": "SQLite（推荐）", "description": "零运维"},
                {"label": "Postgres", "description": "可扩展"}
            ]
        }]})
        .to_string()
    }

    #[tokio::test]
    async fn ask_user_question_blocks_the_turn_until_the_user_answers_and_returns_the_answer_to_the_model()
     {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("ask-user", "向用户提问验收");
        harness.provider.set_tool_then_completed(
            "ask_user_question",
            ask_user_question_arguments(),
            "已按你的选择继续",
        );
        let accepted = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "先问我用哪种数据库，再继续",
                "harness-ask-user-question",
                "harness-ask-user-question-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("提问 Turn 应被接纳");
        let turn_id = accepted.turn_id.clone().expect("应有 Turn");
        let pending = wait_for_pending_user_question(&harness, &session_id).await;
        assert_eq!(pending.questions[0].header, "数据库");
        assert_eq!(pending.questions[0].options.len(), 2);

        // 等待期间这一轮不会结束。
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            harness
                .state
                .session_store
                .canonical_turn_for_session_turn_id(&session_id, &turn_id)
                .is_some_and(|turn| !turn.status.is_terminal()),
            "没有回答之前 Turn 必须保持进行中"
        );

        harness
            .state
            .conversation_registry
            .user_questions()
            .resolve(
                &session_id,
                &pending.question_id,
                magi_conversation_runtime::UserQuestionResponse::Answered {
                    answers: vec![magi_conversation_runtime::UserQuestionAnswer {
                        selected: vec!["Postgres".to_string()],
                        other: None,
                    }],
                },
            )
            .expect("回答应被接受");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        // 答案作为工具结果交给了模型的下一次请求。
        let fed_back = harness.provider.requests().iter().any(|request| {
            format!("{request:?}").contains("answered")
                && format!("{request:?}").contains("Postgres")
        });
        assert!(fed_back, "用户的回答必须作为工具结果返回给模型");
    }

    #[tokio::test]
    async fn ask_user_question_releases_the_repository_lease_while_waiting_and_reacquires_it_after_the_answer()
     {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("ask-user-lease", "向用户提问期间让出仓库租约");
        harness.provider.set_tool_then_completed(
            "ask_user_question",
            ask_user_question_arguments(),
            "已按你的选择继续",
        );
        let accepted = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "先问我用哪种数据库，再继续",
                "harness-ask-user-lease",
                "harness-ask-user-lease-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("提问 Turn 应被接纳");
        let turn_id = accepted.turn_id.clone().expect("应有 Turn");
        let pending = wait_for_pending_user_question(&harness, &session_id).await;
        let git_common_dir = harness
            .state
            .session_code_contexts
            .get(session_id.as_str())
            .expect("Git session 应有 context")
            .git
            .git_common_dir;
        let coordinator = harness.state.workspace_git_coordinator.clone();

        // 等用户期间不占租约：同一仓库的另一个会话可以开始执行。
        assert!(
            !coordinator.session_holds_execution(session_id.as_str(), &git_common_dir),
            "等待回答期间不应占用仓库执行租约"
        );
        coordinator
            .begin_execution("other-session", &git_common_dir)
            .expect("等待回答期间，同一仓库的其它会话应能开始执行");

        // 用户回答时另一个会话仍占着仓库：这一轮不能带着答案继续，必须等租约空出来。
        harness
            .state
            .conversation_registry
            .user_questions()
            .resolve(
                &session_id,
                &pending.question_id,
                magi_conversation_runtime::UserQuestionResponse::Answered {
                    answers: vec![magi_conversation_runtime::UserQuestionAnswer {
                        selected: vec!["Postgres".to_string()],
                        other: None,
                    }],
                },
            )
            .expect("回答应被接受");
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(
            harness
                .state
                .session_store
                .canonical_turn_for_session_turn_id(&session_id, &turn_id)
                .is_some_and(|turn| !turn.status.is_terminal()),
            "仓库被其它会话占用时，回答后的这一轮应等待而不是继续执行"
        );

        coordinator.end_execution("other-session");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn goal_start_turn_failing_before_the_goal_exists_is_resubmitted_after_backoff() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("goal-start-retry", "目标起始轮失败后自动重提");
        // 起始轮因超时失败（HTTP 调用层已放弃），整轮失败；此时目标还没创建。
        harness.provider.set_transient_failures_then_completed(
            "目标已建立",
            "provider request timed out",
            1,
        );
        let accepted = harness
            .submit_workspace_goal_start(
                &session_id,
                &workspace_id,
                &workspace_root,
                "目标：整理文档",
                "harness-goal-start",
            )
            .await
            .expect("目标起始轮应被接纳");
        let first_turn_id = accepted.turn_id.clone().expect("应有 Turn");
        let first = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        assert_eq!(first.status, CanonicalTurnStatus::Failed);
        assert!(
            harness
                .state
                .session_store
                .current_unfinished_goal(&session_id)
                .is_none(),
            "模型在创建目标前就失败，不应已有目标"
        );

        // 退避（5s）后原请求被重新排队并开始新的一轮，用户不必手动重发。
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let retried_turn = loop {
            let retried = harness
                .state
                .session_store
                .canonical_turns_for_session(&session_id)
                .into_iter()
                .find(|turn| turn.turn_id != first_turn_id && !turn.is_session_command());
            if let Some(turn) = retried {
                break turn;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "目标起始轮应在退避后自动重提"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        assert_eq!(
            harness
                .state
                .session_store
                .canonical_turn_for_request_id("harness-goal-start-goalstart-retry-1")
                .map(|turn| turn.turn_id),
            Some(retried_turn.turn_id),
            "重提的请求要带重试序号，防止无限重提并保持幂等"
        );
    }

    #[tokio::test]
    async fn goal_start_turn_is_not_resubmitted_for_a_non_retryable_failure() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("goal-start-no-retry", "目标起始轮的配置错误不重提");
        harness
            .provider
            .set_failure("provider rejected request: model_not_found (http_status=404)");
        let accepted = harness
            .submit_workspace_goal_start(
                &session_id,
                &workspace_id,
                &workspace_root,
                "目标：整理文档",
                "harness-goal-start-no-retry",
            )
            .await
            .expect("目标起始轮应被接纳");
        let first_turn_id = accepted.turn_id.clone().expect("应有 Turn");
        let first = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        assert_eq!(
            first
                .items
                .iter()
                .find_map(|item| item.metadata.get("modelFailure"))
                .and_then(|failure| failure.get("retryable"))
                .and_then(serde_json::Value::as_bool),
            Some(false),
            "前提：模型不存在属于用户不可重试的确定性错误"
        );
        tokio::time::sleep(Duration::from_secs(7)).await;
        let turns = harness
            .state
            .session_store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .filter(|turn| !turn.is_session_command())
            .count();
        assert_eq!(turns, 1, "确定性错误重提只会重复失败，不应自动重提");
        assert!(
            harness.state.goal_start_requests.lock().unwrap().is_empty(),
            "终态收口后不应残留登记"
        );
    }

    #[tokio::test]
    async fn turn_waits_for_a_repository_busy_with_another_session_instead_of_failing() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("repo-busy", "仓库被其它会话占用时排队");
        harness.provider.set_completed_response("轮到我了");
        let git_common_dir = workspace_root.join(".git");
        let coordinator = harness.state.workspace_git_coordinator.clone();
        coordinator
            .begin_execution("other-session", &git_common_dir)
            .expect("其它会话先占用仓库");

        let accepted = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "读一下项目说明",
                "harness-repo-busy",
                "harness-repo-busy-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Turn 应被接纳");
        let turn_id = accepted.turn_id.clone().expect("应有 Turn");

        // 仓库被占用期间这一轮排队等待，不能变成失败的 Turn。
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(
            harness
                .state
                .session_store
                .canonical_turn_for_session_turn_id(&session_id, &turn_id)
                .is_some_and(|turn| !turn.status.is_terminal()),
            "仓库被其它会话占用时，新一轮应排队等待而不是直接失败"
        );
        // 等待期间界面要能说明「正在等谁」。
        let waiting = harness
            .events_for(&session_id)
            .into_iter()
            .find(|event| event.event_type == "session.workspace.waiting")
            .expect("开始排队时应发布 session.workspace.waiting");
        assert_eq!(
            waiting.payload["blocking_session_ids"],
            serde_json::json!(["other-session"])
        );

        // 页面刷新后事件已经错过，靠快照还原「正在等谁」。
        assert_eq!(
            harness
                .state
                .session_workspace_waits
                .lock()
                .unwrap()
                .get(session_id.as_str())
                .cloned(),
            Some(vec!["other-session".to_string()])
        );

        coordinator.end_execution("other-session");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert!(
            harness
                .state
                .session_workspace_waits
                .lock()
                .unwrap()
                .is_empty(),
            "拿到仓库后等待记录要清掉"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .any(|event| event.event_type == "session.workspace.ready"),
            "拿到仓库后应发布 session.workspace.ready"
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn tool_approval_wait_releases_the_repository_lease_and_reacquires_it_before_running() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("approval-lease", "授权等待期间让出仓库租约");
        let target = workspace_root.join("approval-lease.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf approved > {}", target.display())
            })
            .to_string(),
            "授权后完成",
        );
        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "调用 shell_exec 执行一个命令并在审批后汇总结果",
                "harness-approval-lease",
                "harness-approval-lease-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("授权 Turn 应被接纳");
        let turn_id = response.turn_id.clone().expect("应有 Turn");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        let git_common_dir = harness
            .state
            .session_code_contexts
            .get(session_id.as_str())
            .expect("Git session 应有 context")
            .git
            .git_common_dir;
        let coordinator = harness.state.workspace_git_coordinator.clone();
        assert!(
            !coordinator.session_holds_execution(session_id.as_str(), &git_common_dir),
            "等待授权期间不应占用仓库执行租约"
        );
        coordinator
            .begin_execution("other-session", &git_common_dir)
            .expect("等待授权期间，同一仓库的其它会话应能开始执行");

        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
            &pending.approval_id,
            "allow_once",
        )
        .await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(
            !target.exists(),
            "仓库被其它会话占用时，获批的操作不能越过租约直接执行"
        );

        coordinator.end_execution("other-session");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(
            fs::read_to_string(&target).expect("授权后的写入应完成"),
            "approved"
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    fn register_plain_workspace(
        harness: &MagiTurnHarness,
        label: &str,
        files: &[(&str, &str)],
    ) -> (tempfile::TempDir, magi_core::WorkspaceId) {
        let workspace_root = tempfile::tempdir().expect("workspace should create");
        for (path, content) in files {
            fs::write(workspace_root.path().join(path), content).expect("fixture file");
        }
        let workspace_id = magi_core::WorkspaceId::new(format!("harness-isolation-{label}"));
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("workspace should register");
        (workspace_root, workspace_id)
    }

    fn create_workspace_session(
        harness: &MagiTurnHarness,
        workspace_id: &magi_core::WorkspaceId,
        session: &str,
    ) -> SessionId {
        let session_id = SessionId::new(session);
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                session,
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        session_id
    }

    #[tokio::test]
    async fn isolated_session_works_in_its_own_copy_until_the_user_merges() {
        let harness = MagiTurnHarness::new_task("隔离副本完成");
        let (workspace_root, workspace_id) =
            register_plain_workspace(&harness, "merge", &[("a.txt", "original")]);
        let session_id =
            create_workspace_session(&harness, &workspace_id, "isolated-merge-session");
        let isolation = harness
            .state
            .enable_session_isolation(
                &session_id,
                &workspace_id,
                magi_session_isolation::IsolationOrigin::Manual,
            )
            .await
            .expect("手动启用隔离副本");
        assert_ne!(isolation.root, workspace_root.path());
        assert_eq!(
            fs::read_to_string(isolation.root.join("a.txt")).unwrap(),
            "original"
        );

        // 命令用相对路径：cwd 必须是隔离副本，而不是主工作区。
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({ "command": "printf changed > a.txt && printf created > b.txt" })
                .to_string(),
            "隔离副本里已修改",
        );
        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 修改文件",
                "harness-isolation-merge",
                "harness-isolation-merge-user",
                Some(AccessProfile::FullAccess),
            )
            .await
            .expect("隔离会话的 Turn 应被接纳");
        let turn_id = response.turn_id.clone().expect("应有 Turn");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);

        // 主工作区原封不动，改动都在副本里。
        assert_eq!(
            fs::read_to_string(workspace_root.path().join("a.txt")).unwrap(),
            "original"
        );
        assert!(!workspace_root.path().join("b.txt").exists());
        assert_eq!(
            fs::read_to_string(isolation.root.join("a.txt")).unwrap(),
            "changed"
        );
        // 隔离会话没有主工作区的 Git 上下文，也不占执行租约。
        assert!(
            harness
                .state
                .session_code_contexts
                .get(session_id.as_str())
                .is_none()
        );

        let plan = harness
            .state
            .isolation_merge_plan(&session_id)
            .await
            .unwrap();
        assert_eq!(plan.entries.len(), 2, "{plan:?}");
        assert_eq!(plan.count(magi_session_isolation::MergeState::Clean), 2);

        let outcome = harness
            .state
            .isolation_merge_apply(
                &session_id,
                magi_session_isolation::MergeSelection::default(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.applied.len(), 2, "{outcome:?}");
        assert_eq!(
            fs::read_to_string(workspace_root.path().join("a.txt")).unwrap(),
            "changed"
        );
        assert_eq!(
            fs::read_to_string(workspace_root.path().join("b.txt")).unwrap(),
            "created"
        );
        assert!(
            harness
                .state
                .isolation_merge_plan(&session_id)
                .await
                .unwrap()
                .entries
                .is_empty(),
            "合并后基线推进，不再有待合并改动"
        );

        harness
            .state
            .discard_session_isolation(&session_id)
            .await
            .expect("丢弃隔离副本");
        assert!(harness.state.session_isolation(&session_id).is_none());
        assert!(!isolation.root.exists(), "副本目录应被删除");
        assert!(
            workspace_root.path().join("b.txt").exists(),
            "丢弃副本不影响已合并的主工作区"
        );
    }

    #[tokio::test]
    async fn second_session_is_isolated_automatically_when_the_workspace_is_busy() {
        let harness = MagiTurnHarness::new_task("自动隔离完成");
        let (workspace_root, workspace_id) =
            register_plain_workspace(&harness, "contention", &[("a.txt", "original")]);
        let busy = create_workspace_session(&harness, &workspace_id, "contention-busy-session");
        let second = create_workspace_session(&harness, &workspace_id, "contention-second-session");
        // busy 会话在主工作区里有一轮仍在运行。
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &harness.state.session_store,
            harness.state.turn_coordinator(),
            &busy,
            "turn-busy",
            1,
            magi_core::UtcMillis(1_777_000_000_100),
            "running",
            "另一个会话正在执行",
        );

        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({ "command": "printf second > a.txt" }).to_string(),
            "第二个会话已完成",
        );
        let response = harness
            .submit_workspace_task_with_access_profile(
                &second,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 修改文件",
                "harness-isolation-contention",
                "harness-isolation-contention-user",
                Some(AccessProfile::FullAccess),
            )
            .await
            .expect("第二个会话的 Turn 应被接纳");
        let turn_id = response.turn_id.clone().expect("应有 Turn");
        let turn = harness.wait_for_terminal(&second, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);

        let isolation = harness
            .state
            .session_isolation(&second)
            .expect("工作区被占用时第二个会话应被自动隔离");
        assert_eq!(
            isolation.origin,
            magi_session_isolation::IsolationOrigin::Contention {
                blocking_session_id: busy.as_str().to_string()
            }
        );
        assert_eq!(
            fs::read_to_string(workspace_root.path().join("a.txt")).unwrap(),
            "original",
            "自动隔离的会话不能碰主工作区"
        );
        assert_eq!(
            fs::read_to_string(isolation.root.join("a.txt")).unwrap(),
            "second"
        );
        assert!(
            harness.state.session_isolation(&busy).is_none(),
            "正在执行的会话不受影响"
        );
        let _ = fs::remove_dir_all(isolation.root.parent().unwrap());
    }

    #[tokio::test]
    async fn git_workspace_contention_isolates_instead_of_waiting_for_the_repository_lease() {
        let (harness, workspace_id, workspace_root, second) =
            prepare_git_approval_case("isolate-git", "Git 工作区被占用时自动隔离");
        let busy = create_workspace_session(&harness, &workspace_id, "git-contention-busy-session");
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &harness.state.session_store,
            harness.state.turn_coordinator(),
            &busy,
            "turn-git-busy",
            1,
            magi_core::UtcMillis(1_777_000_000_200),
            "running",
            "另一个会话正在执行",
        );
        let git_common_dir = workspace_root.join(".git");
        harness
            .state
            .workspace_git_coordinator
            .begin_execution(busy.as_str(), &git_common_dir)
            .expect("busy 会话占着仓库租约");

        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({ "command": "printf isolated > notes.txt && git status --short" })
                .to_string(),
            "隔离副本里的 Git 可用",
        );
        let response = harness
            .submit_workspace_task_with_access_profile(
                &second,
                &workspace_id,
                &workspace_root,
                "调用 shell_exec 写文件并查看 git 状态",
                "harness-isolation-git",
                "harness-isolation-git-user",
                Some(AccessProfile::FullAccess),
            )
            .await
            .expect("Turn 应被接纳");
        let turn_id = response.turn_id.clone().expect("应有 Turn");
        // 不需要等 busy 会话释放仓库：隔离后直接完成。
        let turn = harness.wait_for_terminal(&second, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);

        let isolation = harness
            .state
            .session_isolation(&second)
            .expect("应被自动隔离");
        assert!(
            isolation.git_available,
            "独立的 .git 目录会一起复制，副本里 Git 可用"
        );
        assert_eq!(
            fs::read_to_string(isolation.root.join("notes.txt")).unwrap(),
            "isolated"
        );
        assert!(!workspace_root.join("notes.txt").exists());
        assert!(
            !harness
                .state
                .workspace_git_coordinator
                .session_holds_execution(second.as_str(), &git_common_dir),
            "隔离会话不占用主工作区的仓库租约"
        );
        harness
            .state
            .workspace_git_coordinator
            .end_execution(busy.as_str());
        let _ = fs::remove_dir_all(isolation.root.parent().unwrap());
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn ask_user_question_is_cancelled_when_the_turn_is_interrupted() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("ask-user-stop", "向用户提问被停止");
        harness.provider.set_tool_then_completed(
            "ask_user_question",
            ask_user_question_arguments(),
            "不应走到这里",
        );
        let accepted = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "先问我用哪种数据库，再继续",
                "harness-ask-user-stop",
                "harness-ask-user-stop-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("提问 Turn 应被接纳");
        let turn_id = accepted.turn_id.clone().expect("应有 Turn");
        let pending = wait_for_pending_user_question(&harness, &session_id).await;

        harness
            .cancel_with_workspace(&session_id, Some(&workspace_id))
            .await
            .expect("停止应被接受");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_ne!(turn.status, CanonicalTurnStatus::Completed);
        for _ in 0..100 {
            if harness
                .state
                .conversation_registry
                .user_questions()
                .pending_for_session(&session_id)
                .is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            harness
                .state
                .conversation_registry
                .user_questions()
                .pending_for_session(&session_id)
                .is_empty(),
            "轮次停止后待回答的问题必须被清掉：{}",
            pending.question_id
        );
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_switch_allow_for_turn_does_not_cross_turn() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("cross-turn", "Git 分支跨 Turn 授权隔离验收");
        let first_provider_requests_before = non_classifier_provider_request_count(&harness);
        harness.provider.set_tool_then_completed(
            "git_branch_switch",
            serde_json::json!({"branch": "approval-target"}).to_string(),
            "第一轮 Git 分支已切换",
        );

        let first = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "调用 git_branch_switch 切换到 approval-target，等待本轮授权后完成",
                "harness-git-approval-cross-turn-first",
                "harness-git-approval-cross-turn-user-1",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 跨 Turn 首轮应被接纳");
        let first_turn_id = first.turn_id.clone().expect("首轮应有 Turn");
        let first_pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
            &first_pending.approval_id,
            "allow_for_turn",
        )
        .await;
        let first_turn = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        assert_eq!(first_turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(current_git_branch(&workspace_root), "approval-target");
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "allow_for_turn_initial_turn",
                "tool": "git_branch_switch",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "allow_for_turn",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness)
                    - first_provider_requests_before,
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "branch_switched",
            }),
            &first_turn,
        );

        let second_provider_requests_before = non_classifier_provider_request_count(&harness);
        harness.provider.set_tool_then_completed(
            "git_branch_switch",
            serde_json::json!({"branch": "main"}).to_string(),
            "第二轮 Git 分支不会绕过审批",
        );
        let second = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "调用 git_branch_switch 切回 main，必须重新等待审批",
                "harness-git-approval-cross-turn-second",
                "harness-git-approval-cross-turn-user-2",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 跨 Turn 第二轮应被接纳");
        let second_turn_id = second.turn_id.clone().expect("第二轮应有 Turn");
        let second_pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_ne!(first_pending.approval_id, second_pending.approval_id);
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            2,
            "allow_for_turn 不得跨 Turn 复用 Git 授权"
        );
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
            &second_pending.approval_id,
            "deny",
        )
        .await;
        let second_turn = harness
            .wait_for_terminal(&session_id, &second_turn_id)
            .await;
        // 第二轮的拒绝只结束这次调用，模型拿到结果后完成本轮（P1-9）。
        assert_eq!(second_turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(current_git_branch(&workspace_root), "approval-target");
        let second_provider_requests =
            non_classifier_provider_request_count(&harness) - second_provider_requests_before;
        assert_eq!(second_provider_requests, 2);
        record_git_approval_matrix_row(
            &second_turn,
            "cross_turn",
            "allow_for_turn_cross_turn",
            "main",
            &current_git_branch(&workspace_root),
            true,
            true,
            second_provider_requests,
            "completed",
            "completed",
            "branch_unchanged",
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_switch_allow_for_turn_does_not_cross_session() {
        let (harness, workspace_id, workspace_root, first_session_id) =
            prepare_git_approval_case("cross-session", "Git 分支跨 Session 授权隔离验收");
        let first_provider_requests_before = non_classifier_provider_request_count(&harness);
        harness.provider.set_tool_then_completed(
            "git_branch_switch",
            serde_json::json!({"branch": "approval-target"}).to_string(),
            "第一 Session Git 分支已切换",
        );

        let first = harness
            .submit_workspace_task_with_access_profile(
                &first_session_id,
                &workspace_id,
                &workspace_root,
                "调用 git_branch_switch 切换到 approval-target，等待本轮授权后完成",
                "harness-git-approval-cross-session-first",
                "harness-git-approval-cross-session-user-1",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("首个 Session 应被接纳");
        let first_turn_id = first.turn_id.clone().expect("首个 Session 应有 Turn");
        let first_pending = wait_for_pending_tool_approval(&harness, &first_session_id).await;
        resolve_tool_approval_via_http(
            &harness,
            &first_session_id,
            &workspace_id,
            &workspace_root,
            &first_pending.approval_id,
            "allow_for_turn",
        )
        .await;
        let first_turn = harness
            .wait_for_terminal(&first_session_id, &first_turn_id)
            .await;
        assert_eq!(first_turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(current_git_branch(&workspace_root), "approval-target");
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "allow_for_turn_initial_session",
                "tool": "git_branch_switch",
                "surface": "git_workspace",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "allow_for_turn",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness)
                    - first_provider_requests_before,
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "branch_switched",
            }),
            &first_turn,
        );

        let second_provider_requests_before = non_classifier_provider_request_count(&harness);
        let second_session_id = SessionId::new("harness-git-approval-cross-session-second");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                second_session_id.clone(),
                "Git 分支跨 Session 第二会话",
                Some(workspace_id.to_string()),
            )
            .expect("第二 Session 应创建");
        harness.provider.set_tool_then_completed(
            "git_branch_switch",
            serde_json::json!({"branch": "main"}).to_string(),
            "第二 Session 不应绕过 Git 审批",
        );
        let second = harness
            .submit_workspace_task_with_access_profile(
                &second_session_id,
                &workspace_id,
                &workspace_root,
                "调用 git_branch_switch 切回 main，必须重新等待审批",
                "harness-git-approval-cross-session-second",
                "harness-git-approval-cross-session-user-2",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("第二 Session 应被接纳");
        let second_turn_id = second.turn_id.clone().expect("第二 Session 应有 Turn");
        let second_pending = wait_for_pending_tool_approval(&harness, &second_session_id).await;
        assert_ne!(first_pending.approval_id, second_pending.approval_id);
        assert_eq!(
            harness
                .events_for(&second_session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1,
            "allow_for_turn 不得跨 Session 复用 Git 授权"
        );
        resolve_tool_approval_via_http(
            &harness,
            &second_session_id,
            &workspace_id,
            &workspace_root,
            &second_pending.approval_id,
            "deny",
        )
        .await;
        let second_turn = harness
            .wait_for_terminal(&second_session_id, &second_turn_id)
            .await;
        // 第二轮的拒绝只结束这次调用，模型拿到结果后完成本轮（P1-9）。
        assert_eq!(second_turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(current_git_branch(&workspace_root), "approval-target");
        let second_provider_requests =
            non_classifier_provider_request_count(&harness) - second_provider_requests_before;
        record_git_approval_matrix_row(
            &second_turn,
            "cross_session",
            "allow_for_turn_cross_session",
            "main",
            &current_git_branch(&workspace_root),
            true,
            true,
            second_provider_requests,
            "completed",
            "completed",
            "branch_unchanged",
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_switch_denial_preserves_branch_and_completes_turn() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("deny", "Git 分支审批拒绝验收");
        harness.provider.set_tool_then_completed(
            "git_branch_switch",
            serde_json::json!({"branch": "approval-target"}).to_string(),
            "不会执行被拒绝的 Git 分支切换",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "调用 git_branch_switch 切换到 approval-target，但必须等待用户审批",
                "harness-git-approval-deny-request",
                "harness-git-approval-deny-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 分支拒绝请求应被接纳");
        let turn_id = response.turn_id.clone().expect("拒绝请求应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("拒绝请求应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        resolve_tool_approval_via_http(
            &harness,
            &session_id,
            &workspace_id,
            &workspace_root,
            &pending.approval_id,
            "deny",
        )
        .await;

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        // 拒绝或过期只结束这次调用，模型拿到结果后完成本轮（P1-9）。
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            current_git_branch(&workspace_root),
            "main",
            "拒绝审批不得切换真实 Git 分支"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "git_branch_switch"
                        && tool
                            .error
                            .as_deref()
                            .is_some_and(|error| error.contains("拒绝"))
                })
        }));
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "拒绝或过期的结果交给模型后只再请求一次以完成本轮"
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            1
        );
        record_git_approval_matrix_row(
            &turn,
            "deny",
            "deny",
            "main",
            &current_git_branch(&workspace_root),
            true,
            true,
            non_classifier_provider_request_count(&harness),
            "completed",
            "completed",
            "branch_unchanged",
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_switch_cancel_preserves_branch_without_resolution() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("cancel", "Git 分支审批取消验收");
        harness.provider.set_tool_then_completed(
            "git_branch_switch",
            serde_json::json!({"branch": "approval-target"}).to_string(),
            "不会执行被取消的 Git 分支切换",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "调用 git_branch_switch 切换到 approval-target，但在审批前取消当前 Turn",
                "harness-git-approval-cancel-request",
                "harness-git-approval-cancel-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 分支取消请求应被接纳");
        let turn_id = response.turn_id.clone().expect("取消请求应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("取消请求应有 root task");
        let _pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        harness
            .cancel_with_workspace(&session_id, Some(&workspace_id))
            .await
            .expect("取消 Git 审批 Turn 应成功");

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Cancelled);
        assert!(matches!(
            task.status,
            magi_core::TaskStatus::Failed | magi_core::TaskStatus::Killed
        ));
        assert_eq!(current_git_branch(&workspace_root), "main");
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty()
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            0,
            "取消未作出审批决定时不得发布 resolved 事件"
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 1);
        record_git_approval_matrix_row(
            &turn,
            "cancel",
            "cancel",
            "main",
            &current_git_branch(&workspace_root),
            true,
            false,
            non_classifier_provider_request_count(&harness),
            "cancelled",
            "failed_or_killed",
            "branch_unchanged",
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_git_branch_switch_expiry_preserves_branch_without_resolution() {
        let (harness, workspace_id, workspace_root, session_id) =
            prepare_git_approval_case("expiry", "Git 分支审批过期验收");
        harness.provider.set_tool_then_completed(
            "git_branch_switch",
            serde_json::json!({"branch": "approval-target"}).to_string(),
            "不会执行已过期的 Git 分支切换",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                &workspace_root,
                "调用 git_branch_switch 切换到 approval-target，但审批过期后不得执行",
                "harness-git-approval-expiry-request",
                "harness-git-approval-expiry-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("Git 分支过期请求应被接纳");
        let turn_id = response.turn_id.clone().expect("过期请求应有 Turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("过期请求应有 root task");
        let _pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .expire_stale(UtcMillis(UtcMillis::now().0 + TOOL_APPROVAL_TTL_MILLIS + 1,)),
            1,
            "测试必须显式推进 Git 审批时钟"
        );

        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;
        // 授权过期是这次调用的结局，交给模型改用其他方式继续，不再让整轮失败。
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(current_git_branch(&workspace_root), "main");
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "git_branch_switch"
                        && tool.error.as_deref().is_some_and(|error| {
                            error.contains("tool_approval_expired") || error.contains("过期")
                        })
                })
        }));
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            0
        );
        assert_eq!(non_classifier_provider_request_count(&harness), 2);
        record_git_approval_matrix_row(
            &turn,
            "expiry",
            "expiry",
            "main",
            &current_git_branch(&workspace_root),
            true,
            false,
            non_classifier_provider_request_count(&harness),
            "completed",
            "completed",
            "branch_unchanged",
        );
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn restricted_profile_duplicate_request_replays_pending_approval_without_duplicate_side_effects()
     {
        let harness = MagiTurnHarness::new_task("重复审批请求回放");
        let workspace_root =
            tempfile::tempdir().expect("duplicate approval workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-duplicate-approval-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("duplicate approval workspace should register");
        let session_id = SessionId::new("harness-duplicate-approval-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "重复审批请求回放",
                Some(workspace_id.to_string()),
            )
            .expect("duplicate approval session should create");
        let target = workspace_root.path().join("duplicate-approval.txt");
        harness.provider.set_tool_then_completed(
            "shell_exec",
            serde_json::json!({
                "command": format!("printf duplicate > {}", target.display())
            })
            .to_string(),
            "重复审批请求最终完成",
        );

        let first = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 写入文件，等待审批后汇总结果",
                "harness-duplicate-approval-request",
                "harness-duplicate-approval-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("首次受限请求应被接纳");
        let first_turn_id = first.turn_id.clone().expect("首次请求应有 Turn");
        let first_task_id = first.root_task_id.clone().expect("首次请求应有 root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;

        let replay = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 shell_exec 写入文件，等待审批后汇总结果",
                "harness-duplicate-approval-request",
                "harness-duplicate-approval-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("相同 requestId/fingerprint 应回放待处理 Turn");
        assert_eq!(replay.turn_id.as_deref(), Some(first_turn_id.as_str()));
        assert_eq!(replay.root_task_id.as_deref(), Some(first_task_id.as_str()));
        assert_eq!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .len(),
            1,
            "重复提交不得创建第二个 pending 审批"
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1,
            "重复提交不得重复发布审批请求事件"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            1,
            "待审批期间重复提交不得重复请求 Provider"
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

        let turn = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(first_task_id))
            .await;
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(&target).expect("审批允许后应产生一次文件副作用"),
            "duplicate"
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            1,
            "重复提交后的审批请求总数仍应为一"
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            1,
            "同一 pending 审批只能收口一次"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "审批放行后只允许原始工具轮和一次最终答复轮"
        );
    }

    #[tokio::test]
    async fn restricted_profile_file_remove_allow_once_deletes_target_without_retry() {
        let harness = MagiTurnHarness::new_task("审批后删除文件");
        let workspace_root = tempfile::tempdir().expect("remove approval workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-remove-approval-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("remove approval workspace should register");
        let session_id = SessionId::new("harness-remove-approval-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "审批删除文件验收",
                Some(workspace_id.to_string()),
            )
            .expect("remove approval session should create");
        let target = workspace_root.path().join("approval-remove.txt");
        fs::write(&target, "delete after approval").expect("remove approval fixture should write");
        harness.provider.set_tool_then_completed(
            "file_remove",
            serde_json::json!({"path": target.display().to_string()}).to_string(),
            "审批删除文件完成",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 file_remove 删除工作区内文件，等待审批后汇总结果",
                "harness-remove-approval-request",
                "harness-remove-approval-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("remove approval task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("remove approval task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("remove approval task should have root task");
        let pending = wait_for_pending_tool_approval(&harness, &session_id).await;
        assert_eq!(pending.tool_name, "file_remove");
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
        assert!(!target.exists(), "allow_once file_remove 应删除目标文件");
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "allow_once file_remove 后不得重复请求 Provider"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .any(|event| event.event_type == "tool.approval.resolved"),
            "allow_once file_remove 必须发布 resolved 事件"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "file_remove" && tool.result.is_some() && tool.error.is_none()
                })
        }));
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "restricted_file_remove_allow_once",
                "tool": "file_remove",
                "surface": "file_executor",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "allow_once",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "file_removed",
            }),
            &turn,
        );
    }

    #[tokio::test]
    async fn restricted_profile_approval_denial_returns_to_model_without_side_effect() {
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
        // 拒绝或过期只结束这次调用，模型拿到结果后完成本轮（P1-9）。
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
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
            2,
            "拒绝或过期的结果交给模型后只再请求一次以完成本轮"
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
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "restricted_shell_write_deny",
                "tool": "shell_exec",
                "surface": "shell_executor",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "deny",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "write_blocked",
            }),
            &turn,
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
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "restricted_shell_write_cancel",
                "tool": "shell_exec",
                "surface": "shell_executor",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "cancel",
                "approval_requested": true,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "cancelled",
                "task_status": "killed",
                "side_effect": "write_blocked",
            }),
            &turn,
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
        // 授权过期只结束这次调用：模型拿到过期结果后继续完成本轮。
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
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
            2,
            "拒绝或过期的结果交给模型后只再请求一次以完成本轮"
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "restricted_shell_write_expiry",
                "tool": "shell_exec",
                "surface": "shell_executor",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "expiry",
                "approval_requested": true,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "write_blocked",
            }),
            &turn,
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
        // 拒绝或过期只结束这次调用，模型拿到结果后完成本轮（P1-9）。
        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert!(target.exists(), "拒绝 file_remove 后目标文件必须保留");
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "拒绝或过期的结果交给模型后只再请求一次以完成本轮"
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
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "restricted_file_remove_deny",
                "tool": "file_remove",
                "surface": "file_executor",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "deny",
                "approval_requested": true,
                "approval_resolved": true,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "file_preserved",
            }),
            &turn,
        );
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
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "restricted_file-patch_internal",
                "tool": "file_patch",
                "surface": "file_executor",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "allow",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&patch_harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "file_patched",
            }),
            &patch_turn,
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "restricted_file-mkdir_internal",
                "tool": "file_mkdir",
                "surface": "file_executor",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "allow",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&mkdir_harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "directory_created",
            }),
            &mkdir_turn,
        );
    }

    #[tokio::test]
    async fn restricted_profile_auto_allows_file_write_inside_workspace() {
        let harness = MagiTurnHarness::new_task("file_write 自动允许后完成");
        let workspace_root = tempfile::tempdir().expect("file write workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-file-write-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("file write workspace should register");
        let session_id = SessionId::new("harness-file-write-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "file_write 自动允许验收",
                Some(workspace_id.to_string()),
            )
            .expect("file write session should create");
        let target = workspace_root.path().join("created.txt");
        harness.provider.set_tool_then_completed(
            "file_write",
            serde_json::json!({
                "path": target.display().to_string(),
                "content": "restricted write"
            })
            .to_string(),
            "file_write 自动允许后完成",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 file_write 写入工作区内文件",
                "harness-file-write-request",
                "harness-file-write-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("file write task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("file write task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("file write task should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;

        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(fs::read_to_string(&target).unwrap(), "restricted write");
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "Restricted 下工作区内 file_write 应执行工具轮和一次最终答复轮"
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.requested")
                .count(),
            0,
            "Restricted 下工作区内 file_write 应自动允许"
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .iter()
                .filter(|event| event.event_type == "tool.approval.resolved")
                .count(),
            0,
            "自动允许的 file_write 不应伪造审批收口"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "file_write" && tool.result.is_some() && tool.error.is_none()
                })
        }));
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "restricted_file_write_internal",
                "tool": "file_write",
                "surface": "file_executor",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "allow",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "file_written",
            }),
            &turn,
        );
    }

    #[tokio::test]
    async fn restricted_profile_auto_allows_file_copy_and_move_inside_workspace() {
        let copy_harness = MagiTurnHarness::new_task("file_copy 自动允许后完成");
        let copy_workspace_root = tempfile::tempdir().expect("file copy workspace should create");
        let copy_workspace_id = magi_core::WorkspaceId::new("harness-file-copy-workspace");
        copy_harness
            .state
            .workspace_registry
            .register_native_path(
                copy_workspace_id.clone(),
                copy_workspace_root.path().to_path_buf(),
            )
            .expect("file copy workspace should register");
        let copy_session_id = SessionId::new("harness-file-copy-session");
        copy_harness
            .state
            .session_store
            .create_session_for_workspace(
                copy_session_id.clone(),
                "file_copy 自动允许验收",
                Some(copy_workspace_id.to_string()),
            )
            .expect("file copy session should create");
        let copy_source = copy_workspace_root.path().join("source.txt");
        let copy_destination = copy_workspace_root.path().join("copied.txt");
        fs::write(&copy_source, "copy content").expect("file copy fixture should write");
        copy_harness.provider.set_tool_then_completed(
            "file_copy",
            serde_json::json!({
                "source": copy_source.display().to_string(),
                "destination": copy_destination.display().to_string()
            })
            .to_string(),
            "file_copy 自动允许后完成",
        );

        let copy_response = copy_harness
            .submit_workspace_task_with_access_profile(
                &copy_session_id,
                &copy_workspace_id,
                copy_workspace_root.path(),
                "调用 file_copy 复制工作区内文件",
                "harness-file-copy-request",
                "harness-file-copy-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("file copy task should be accepted");
        let copy_turn_id = copy_response
            .turn_id
            .clone()
            .expect("file copy task should have turn");
        let copy_task_id = copy_response
            .root_task_id
            .clone()
            .expect("file copy task should have root task");
        let copy_turn = copy_harness
            .wait_for_terminal(&copy_session_id, &copy_turn_id)
            .await;
        let copy_task = copy_harness
            .wait_for_task_terminal(&magi_core::TaskId::new(copy_task_id))
            .await;
        assert_eq!(copy_turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(copy_task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(&copy_destination).unwrap(),
            "copy content"
        );
        assert_eq!(
            non_classifier_provider_request_count(&copy_harness),
            2,
            "Restricted file_copy 应执行工具轮和一次最终答复轮"
        );
        assert!(
            copy_harness
                .events_for(&copy_session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "Restricted 工作区内 file_copy 应自动允许"
        );
        assert!(copy_turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "file_copy" && tool.result.is_some() && tool.error.is_none()
                })
        }));

        let move_harness = MagiTurnHarness::new_task("file_move 自动允许后完成");
        let move_workspace_root = tempfile::tempdir().expect("file move workspace should create");
        let move_workspace_id = magi_core::WorkspaceId::new("harness-file-move-workspace");
        move_harness
            .state
            .workspace_registry
            .register_native_path(
                move_workspace_id.clone(),
                move_workspace_root.path().to_path_buf(),
            )
            .expect("file move workspace should register");
        let move_session_id = SessionId::new("harness-file-move-session");
        move_harness
            .state
            .session_store
            .create_session_for_workspace(
                move_session_id.clone(),
                "file_move 自动允许验收",
                Some(move_workspace_id.to_string()),
            )
            .expect("file move session should create");
        let move_source = move_workspace_root.path().join("move-source.txt");
        let move_destination = move_workspace_root.path().join("move-destination.txt");
        fs::write(&move_source, "move content").expect("file move fixture should write");
        move_harness.provider.set_tool_then_completed(
            "file_move",
            serde_json::json!({
                "source": move_source.display().to_string(),
                "destination": move_destination.display().to_string()
            })
            .to_string(),
            "file_move 自动允许后完成",
        );

        let move_response = move_harness
            .submit_workspace_task_with_access_profile(
                &move_session_id,
                &move_workspace_id,
                move_workspace_root.path(),
                "调用 file_move 移动工作区内文件",
                "harness-file-move-request",
                "harness-file-move-user",
                Some(AccessProfile::Restricted),
            )
            .await
            .expect("file move task should be accepted");
        let move_turn_id = move_response
            .turn_id
            .clone()
            .expect("file move task should have turn");
        let move_task_id = move_response
            .root_task_id
            .clone()
            .expect("file move task should have root task");
        let move_turn = move_harness
            .wait_for_terminal(&move_session_id, &move_turn_id)
            .await;
        let move_task = move_harness
            .wait_for_task_terminal(&magi_core::TaskId::new(move_task_id))
            .await;
        assert_eq!(move_turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(move_task.status, magi_core::TaskStatus::Completed);
        assert!(!move_source.exists());
        assert_eq!(
            fs::read_to_string(&move_destination).unwrap(),
            "move content"
        );
        assert_eq!(
            non_classifier_provider_request_count(&move_harness),
            2,
            "Restricted file_move 应执行工具轮和一次最终答复轮"
        );
        assert!(
            move_harness
                .events_for(&move_session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "Restricted 工作区内 file_move 应自动允许"
        );
        assert!(move_turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "file_move" && tool.result.is_some() && tool.error.is_none()
                })
        }));
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "restricted_file-copy_internal",
                "tool": "file_copy",
                "surface": "file_executor",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "allow",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&copy_harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "file_copied",
            }),
            &copy_turn,
        );
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "restricted_file-move_internal",
                "tool": "file_move",
                "surface": "file_executor",
                "access_profile": "Restricted",
                "scope": "workspace_internal",
                "lifecycle": "allow",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&move_harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "file_moved",
            }),
            &move_turn,
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

        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
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
            2,
            "确定性的路径拒绝后不得重试 Provider"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item
                    .tool
                    .as_ref()
                    .is_some_and(|tool| tool.name == "file_write" && tool.error.is_some())
        }));
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "restricted_external_file-write",
                "tool": "file_write",
                "surface": "file_executor",
                "access_profile": "Restricted",
                "scope": "workspace_external",
                "lifecycle": "deny",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "failed",
                "task_status": "failed",
                "side_effect": "external_path_blocked",
            }),
            &turn,
        );
    }

    #[tokio::test]
    async fn restricted_profile_rejects_file_patch_mkdir_copy_move_and_remove_outside_workspace() {
        let (_patch_workspace, _patch_outside, patch_target, patch_turn, _patch_task) =
            run_restricted_outside_file_tool_case(
                "file_patch",
                "file-patch",
                "调用 file_patch 修改工作区之外的文件",
                |_workspace_root, outside_root| {
                    let target = outside_root.join("outside-patch.txt");
                    fs::write(&target, "before\n").expect("outside patch fixture should write");
                    (
                        target.clone(),
                        serde_json::json!({
                            "path": target.display().to_string(),
                            "old_string": "before",
                            "new_string": "after"
                        }),
                    )
                },
            )
            .await;
        assert_eq!(
            fs::read_to_string(&patch_target).expect("outside patch target should remain"),
            "before\n"
        );
        assert!(patch_turn.status.is_terminal());

        let (_mkdir_workspace, _mkdir_outside, mkdir_target, mkdir_turn, _mkdir_task) =
            run_restricted_outside_file_tool_case(
                "file_mkdir",
                "file-mkdir",
                "调用 file_mkdir 创建工作区之外的目录",
                |_workspace_root, outside_root| {
                    let target = outside_root.join("outside-mkdir").join("nested");
                    (
                        target.clone(),
                        serde_json::json!({"path": target.display().to_string()}),
                    )
                },
            )
            .await;
        assert!(!mkdir_target.exists());
        assert!(mkdir_turn.status.is_terminal());

        let (_copy_workspace, _copy_outside, copy_target, copy_turn, _copy_task) =
            run_restricted_outside_file_tool_case(
                "file_copy",
                "file-copy",
                "调用 file_copy 将文件复制到工作区之外",
                |workspace_root, outside_root| {
                    let source = workspace_root.join("copy-source.txt");
                    let target = outside_root.join("outside-copy.txt");
                    fs::write(&source, "copy source").expect("copy source fixture should write");
                    (
                        target.clone(),
                        serde_json::json!({
                            "source": source.display().to_string(),
                            "destination": target.display().to_string()
                        }),
                    )
                },
            )
            .await;
        assert!(!copy_target.exists());
        assert!(copy_turn.status.is_terminal());

        let (_move_workspace, _move_outside, move_target, move_turn, _move_task) =
            run_restricted_outside_file_tool_case(
                "file_move",
                "file-move",
                "调用 file_move 将文件移动到工作区之外",
                |workspace_root, outside_root| {
                    let source = workspace_root.join("move-source.txt");
                    let target = outside_root.join("outside-move.txt");
                    fs::write(&source, "move source").expect("move source fixture should write");
                    (
                        target.clone(),
                        serde_json::json!({
                            "source": source.display().to_string(),
                            "destination": target.display().to_string()
                        }),
                    )
                },
            )
            .await;
        assert!(!move_target.exists());
        assert!(move_turn.status.is_terminal());

        let (_remove_workspace, _remove_outside, remove_target, remove_turn, _remove_task) =
            run_restricted_outside_file_tool_case(
                "file_remove",
                "file-remove",
                "调用 file_remove 删除工作区之外的文件",
                |_workspace_root, outside_root| {
                    let target = outside_root.join("outside-remove.txt");
                    fs::write(&target, "must remain").expect("remove target fixture should write");
                    (
                        target.clone(),
                        serde_json::json!({"path": target.display().to_string()}),
                    )
                },
            )
            .await;
        assert!(remove_target.exists());
        assert_eq!(
            fs::read_to_string(&remove_target).expect("outside remove target should remain"),
            "must remain"
        );
        assert!(remove_turn.status.is_terminal());
    }

    #[tokio::test]
    async fn restricted_profile_rejects_apply_patch_outside_workspace() {
        let (_workspace, _outside, target, turn, _task) = run_restricted_outside_file_tool_case(
            "apply_patch",
            "apply-patch",
            "调用 apply_patch 新建工作区之外的文件",
            |_workspace_root, outside_root| {
                let target = outside_root.join("outside-apply-patch.txt");
                let patch = format!(
                    "*** Begin Patch\n*** Add File: {}\n+must not create\n*** End Patch\n",
                    target.display()
                );
                (target, serde_json::json!({"patch": patch}))
            },
        )
        .await;
        assert!(!target.exists(), "Restricted apply_patch 不得写入工作区外");
        assert!(turn.status.is_terminal());
    }

    #[tokio::test]
    async fn full_access_profile_allows_write_outside_workspace_without_approval() {
        let harness = MagiTurnHarness::new_task("完全授权允许工作区外写入");
        let workspace_root = tempfile::tempdir().expect("full access workspace should create");
        let outside_root = tempfile::tempdir().expect("outside workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-full-access-path-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("full access workspace should register");
        let session_id = SessionId::new("harness-full-access-path-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "完全授权工作区外路径验收",
                Some(workspace_id.to_string()),
            )
            .expect("full access path session should create");
        let target = outside_root.path().join("escape-full-access.txt");
        harness.provider.set_tool_then_completed(
            "file_write",
            serde_json::json!({
                "path": target.display().to_string(),
                "content": "must stay inside workspace"
            })
            .to_string(),
            "完全授权工作区外写入完成",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 file_write 写入工作区之外的路径并返回结果",
                "harness-full-access-path-request",
                "harness-full-access-path-user",
                Some(AccessProfile::FullAccess),
            )
            .await
            .expect("full access path task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("full access path task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("full access path task should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;

        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "must stay inside workspace"
        );
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "完全授权工作区外写入应执行工具轮和一次最终答复轮"
        );
        assert!(
            harness
                .state
                .turn_coordinator()
                .tool_approvals()
                .pending_for_session(&session_id)
                .is_empty(),
            "完全授权工作区外写入不应创建 pending approval"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "完全授权工作区外写入不应发布审批请求"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "file_write" && tool.result.is_some() && tool.error.is_none()
                })
        }));
        record_turn_permission_matrix_row(
            serde_json::json!({
                "case": "full_access_file_write_external",
                "tool": "file_write",
                "surface": "file_executor",
                "access_profile": "FullAccess",
                "scope": "workspace_external",
                "lifecycle": "allow",
                "approval_requested": false,
                "approval_resolved": false,
                "provider_requests": non_classifier_provider_request_count(&harness),
                "turn_status": "completed",
                "task_status": "completed",
                "side_effect": "external_file_written",
            }),
            &turn,
        );
    }

    #[tokio::test]
    async fn full_access_profile_executes_file_operations_outside_workspace_without_approval() {
        let (_patch_workspace, _patch_outside, patch_target, _, _) =
            run_full_access_external_file_tool_case(
                "file_patch",
                "file-patch",
                "调用 file_patch 修改工作区之外的文件并返回结果",
                "external_file_patched",
                |_workspace_root, outside_root| {
                    let target = outside_root.join("external-patch.txt");
                    fs::write(&target, "before\n").expect("external patch fixture should write");
                    (
                        target.clone(),
                        serde_json::json!({
                            "path": target.display().to_string(),
                            "old_string": "before",
                            "new_string": "after"
                        }),
                    )
                },
            )
            .await;
        assert_eq!(
            fs::read_to_string(&patch_target).expect("external patch target should be readable"),
            "after\n"
        );

        let (_mkdir_workspace, _mkdir_outside, mkdir_target, _, _) =
            run_full_access_external_file_tool_case(
                "file_mkdir",
                "file-mkdir",
                "调用 file_mkdir 创建工作区之外的目录并返回结果",
                "external_directory_created",
                |_workspace_root, outside_root| {
                    let target = outside_root.join("external-mkdir").join("nested");
                    (
                        target.clone(),
                        serde_json::json!({"path": target.display().to_string()}),
                    )
                },
            )
            .await;
        assert!(
            mkdir_target.is_dir(),
            "完全授权 file_mkdir 应创建工作区外目录"
        );

        let (_copy_workspace, _copy_outside, copy_target, _, _) =
            run_full_access_external_file_tool_case(
                "file_copy",
                "file-copy",
                "调用 file_copy 将文件复制到工作区之外并返回结果",
                "external_file_copied",
                |workspace_root, outside_root| {
                    let source = workspace_root.join("external-copy-source.txt");
                    let target = outside_root.join("external-copy-target.txt");
                    fs::write(&source, "copy content")
                        .expect("external copy source fixture should write");
                    (
                        target.clone(),
                        serde_json::json!({
                            "source": source.display().to_string(),
                            "destination": target.display().to_string()
                        }),
                    )
                },
            )
            .await;
        assert_eq!(
            fs::read_to_string(&copy_target).expect("external copy target should be readable"),
            "copy content"
        );

        let (_move_workspace, _move_outside, move_target, _, _) =
            run_full_access_external_file_tool_case(
                "file_move",
                "file-move",
                "调用 file_move 将文件移动到工作区之外并返回结果",
                "external_file_moved",
                |workspace_root, outside_root| {
                    let source = workspace_root.join("external-move-source.txt");
                    let target = outside_root.join("external-move-target.txt");
                    fs::write(&source, "move content")
                        .expect("external move source fixture should write");
                    (
                        target.clone(),
                        serde_json::json!({
                            "source": source.display().to_string(),
                            "destination": target.display().to_string()
                        }),
                    )
                },
            )
            .await;
        assert_eq!(
            fs::read_to_string(&move_target).expect("external move target should be readable"),
            "move content"
        );

        let (_remove_workspace, _remove_outside, remove_target, _, _) =
            run_full_access_external_file_tool_case(
                "file_remove",
                "file-remove",
                "调用 file_remove 删除工作区之外的文件并返回结果",
                "external_file_removed",
                |_workspace_root, outside_root| {
                    let target = outside_root.join("external-remove.txt");
                    fs::write(&target, "remove me").expect("external remove fixture should write");
                    (
                        target.clone(),
                        serde_json::json!({"path": target.display().to_string()}),
                    )
                },
            )
            .await;
        assert!(
            !remove_target.exists(),
            "完全授权 file_remove 应删除工作区外文件"
        );
    }

    #[tokio::test]
    async fn full_access_profile_executes_file_remove_without_approval() {
        let harness = MagiTurnHarness::new_task("完全授权删除文件");
        let workspace_root =
            tempfile::tempdir().expect("full access remove workspace should create");
        let workspace_id = magi_core::WorkspaceId::new("harness-full-access-remove-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace_id.clone(), workspace_root.path().to_path_buf())
            .expect("full access remove workspace should register");
        let session_id = SessionId::new("harness-full-access-remove-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "完全授权删除文件验收",
                Some(workspace_id.to_string()),
            )
            .expect("full access remove session should create");
        let target = workspace_root.path().join("full-access-remove.txt");
        fs::write(&target, "remove me").expect("full access remove fixture should write");
        harness.provider.set_tool_then_completed(
            "file_remove",
            serde_json::json!({"path": target.display().to_string()}).to_string(),
            "完全授权删除文件完成",
        );

        let response = harness
            .submit_workspace_task_with_access_profile(
                &session_id,
                &workspace_id,
                workspace_root.path(),
                "调用 file_remove 删除工作区内文件并返回结果",
                "harness-full-access-remove-request",
                "harness-full-access-remove-user",
                Some(AccessProfile::FullAccess),
            )
            .await
            .expect("full access remove task should be accepted");
        let turn_id = response
            .turn_id
            .clone()
            .expect("full access remove task should have turn");
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("full access remove task should have root task");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        let task = harness
            .wait_for_task_terminal(&magi_core::TaskId::new(root_task_id))
            .await;

        assert_eq!(turn.status, CanonicalTurnStatus::Completed);
        assert_eq!(task.status, magi_core::TaskStatus::Completed);
        assert!(!target.exists(), "完全授权 file_remove 应删除目标文件");
        assert_eq!(
            non_classifier_provider_request_count(&harness),
            2,
            "完全授权 file_remove 应执行工具轮和一次最终答复轮"
        );
        assert!(
            harness
                .events_for(&session_id)
                .iter()
                .all(|event| event.event_type != "tool.approval.requested"),
            "完全授权 file_remove 不应发布审批请求"
        );
        assert!(turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::ToolCall
                && item.tool.as_ref().is_some_and(|tool| {
                    tool.name == "file_remove" && tool.result.is_some() && tool.error.is_none()
                })
        }));
    }

    #[tokio::test]
    async fn full_access_profile_executes_patch_mkdir_copy_and_move_without_approval() {
        let (_patch_workspace, patch_target, _patch_turn, _patch_task) =
            run_full_access_file_tool_case(
                "file_patch",
                "file-patch",
                "调用 file_patch 修改文件并返回结果",
                |workspace_root| {
                    let target = workspace_root.join("full-access-patch.txt");
                    fs::write(&target, "before\n").expect("full access patch fixture should write");
                    (
                        target.clone(),
                        serde_json::json!({
                            "path": target.display().to_string(),
                            "old_string": "before",
                            "new_string": "after"
                        }),
                    )
                },
            )
            .await;
        assert_eq!(fs::read_to_string(&patch_target).unwrap(), "after\n");

        let (_mkdir_workspace, mkdir_target, _mkdir_turn, _mkdir_task) =
            run_full_access_file_tool_case(
                "file_mkdir",
                "file-mkdir",
                "调用 file_mkdir 创建目录并返回结果",
                |workspace_root| {
                    let target = workspace_root.join("full-access-mkdir").join("nested");
                    (
                        target.clone(),
                        serde_json::json!({"path": target.display().to_string()}),
                    )
                },
            )
            .await;
        assert!(mkdir_target.is_dir());

        let (_copy_workspace, copy_target, _copy_turn, _copy_task) =
            run_full_access_file_tool_case(
                "file_copy",
                "file-copy",
                "调用 file_copy 复制文件并返回结果",
                |workspace_root| {
                    let source = workspace_root.join("full-access-copy-source.txt");
                    let target = workspace_root.join("full-access-copy-target.txt");
                    fs::write(&source, "copy content")
                        .expect("full access copy fixture should write");
                    (
                        target.clone(),
                        serde_json::json!({
                            "source": source.display().to_string(),
                            "destination": target.display().to_string()
                        }),
                    )
                },
            )
            .await;
        assert_eq!(fs::read_to_string(&copy_target).unwrap(), "copy content");

        let (_move_workspace, move_target, _move_turn, _move_task) =
            run_full_access_file_tool_case(
                "file_move",
                "file-move",
                "调用 file_move 移动文件并返回结果",
                |workspace_root| {
                    let source = workspace_root.join("full-access-move-source.txt");
                    let target = workspace_root.join("full-access-move-target.txt");
                    fs::write(&source, "move content")
                        .expect("full access move fixture should write");
                    (
                        target.clone(),
                        serde_json::json!({
                            "source": source.display().to_string(),
                            "destination": target.display().to_string()
                        }),
                    )
                },
            )
            .await;
        assert_eq!(fs::read_to_string(&move_target).unwrap(), "move content");
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
        let (workspace_id, workspace_root) = register_git_workspace(&harness);
        let session_id = SessionId::new("harness-agent-spawn-many-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "多子代理验收",
                Some(workspace_id.to_string()),
            )
            .expect("多子代理 session 应创建");
        harness
            .state
            .ensure_session_code_context(&session_id, &Some(workspace_id.clone()))
            .await
            .expect("多子代理场景应使用 Git 隔离工作区");
        let response = harness
            .submit_workspace_task(
                &session_id,
                &workspace_id,
                &workspace_root,
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
        let _ = fs::remove_dir_all(workspace_root);
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

        // canonical store 会在终态事件发布前立即更新。并行测试负载下，
        // wait_for_terminal 可能先观察到这个短窗口，因此先等待终态事件投影，
        // 再构造本用例要验证的重连快照。
        let mut terminal_event_published = false;
        for _ in 0..200 {
            terminal_event_published = harness.events_for(&session_id).iter().any(|event| {
                event.event_type == "session.turn.item"
                    && event
                        .payload
                        .get("canonical_turn")
                        .and_then(|turn| turn.get("status"))
                        .and_then(serde_json::Value::as_str)
                        == Some("completed")
            });
            if terminal_event_published {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            terminal_event_published,
            "Turn 终态提交后必须发布 completed canonical event；events={:#?}",
            harness.events_for(&session_id)
        );

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
            event.event_type == "session.turn.item" && event.payload.get("stream_delta").is_some()
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
    async fn task_profile_rejects_steer_without_replacing_the_active_turn() {
        let harness = MagiTurnHarness::new_task("不会先完成");
        harness.provider.set_hold_for_cancellation();
        let initial = harness
            .submit(
                None,
                "请执行一个任务并保持等待",
                "harness-steer-root-request",
                "harness-steer-root-user",
            )
            .await
            .expect("可取消的 task Turn 应先被接纳");
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
        let steer_result = tokio::time::timeout(
            Duration::from_secs(2),
            harness.steer(
                &session_id,
                &turn_id,
                "优先收口当前响应",
                "harness-steer-request",
                "harness-steer-user",
            ),
        )
        .await
        .expect("任务模式 steer 必须在有限时间内返回");
        assert!(
            matches!(&steer_result, Err(crate::errors::ApiError::TurnConflict { conflict_kind, message, .. }) if conflict_kind == "steer_unsupported" || message.contains("任务模式不支持")),
            "任务模式 steer 应明确拒绝: {steer_result:?}"
        );
        harness
            .cancel(&session_id)
            .await
            .expect("任务 steer 场景应可取消");
        let terminal = harness.wait_for_terminal(&session_id, &turn_id).await;
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
        let root_task_id = response
            .root_task_id
            .clone()
            .expect("task profile 应有 root task");
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
        let provider_request_count_before_cancel = harness.provider.requests().len();
        harness.cancel(&session_id).await.expect("取消应成功");
        assert!(
            harness
                .state
                .runner_manager()
                .is_some_and(|manager| manager.status(&root_task_id).is_none()),
            "取消响应返回前必须等待 Runner 和 in-flight dispatch quiescent"
        );
        assert_eq!(
            harness.provider.requests().len(),
            provider_request_count_before_cancel,
            "settlement 后不得再启动该 Turn 的 Provider 请求"
        );
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
    async fn immediate_task_turn_cancel_settles_before_next_turn_is_accepted() {
        let harness = MagiTurnHarness::new_task("取消后的请求完成");
        harness.provider.set_hold_for_cancellation();
        let accepted = harness
            .submit_task(
                "请执行一个任务：在 Runner 启动交接期间允许立即取消",
                "harness-immediate-cancel-request",
                "harness-immediate-cancel-user",
            )
            .await
            .expect("Task Turn 应先被可靠接纳");
        let session_id = SessionId::new(accepted.session_id.clone());
        let first_turn_id = accepted.turn_id.clone().expect("首轮 Turn 应有身份");
        let root_task_id = accepted
            .root_task_id
            .clone()
            .expect("Task profile 应有 root task");

        harness
            .cancel(&session_id)
            .await
            .expect("Runner lease 交接期间的取消必须收口成功");

        let cancelled = harness.wait_for_terminal(&session_id, &first_turn_id).await;
        assert_eq!(cancelled.status, CanonicalTurnStatus::Cancelled);
        assert!(
            harness
                .state
                .runner_manager()
                .is_some_and(|manager| manager.status(&root_task_id).is_none())
        );
        let task_store = harness
            .state
            .task_store()
            .expect("Task harness 应有 TaskStore");
        let root_task = task_store
            .get_task(&magi_core::TaskId::new(root_task_id.clone()))
            .expect("取消后的 root task 应保留历史事实");
        assert_eq!(root_task.status, magi_core::TaskStatus::Killed);
        assert!(
            task_store.get_active_lease(&root_task.task_id).is_none(),
            "取消收口后 root task 不得保留活跃 lease"
        );
        assert_eq!(
            harness
                .events_for(&session_id)
                .into_iter()
                .filter(|event| event.event_type == "session.turn.item")
                .filter(|event| {
                    event
                        .payload
                        .get("canonical_turn")
                        .and_then(|turn| turn.get("turnId"))
                        .and_then(serde_json::Value::as_str)
                        == Some(first_turn_id.as_str())
                        && event
                            .payload
                            .get("canonical_turn")
                            .and_then(|turn| turn.get("status"))
                            .and_then(serde_json::Value::as_str)
                            == Some("cancelled")
                })
                .count(),
            1,
            "立即取消只能发布一个 canonical 终态"
        );

        harness.provider.set_completed_response("取消后的请求完成");
        let next = harness
            .submit(
                Some(&session_id),
                "请执行一个任务：验证取消结算后同一 Session 可以启动下一轮",
                "harness-immediate-cancel-next-request",
                "harness-immediate-cancel-next-user",
            )
            .await
            .expect("取消结算后下一 Turn 应可接纳");
        let next_turn_id = next.turn_id.expect("下一 Turn 应有身份");
        let completed = harness.wait_for_terminal(&session_id, &next_turn_id).await;
        assert_eq!(completed.status, CanonicalTurnStatus::Completed);
        assert!(completed.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item.content.as_deref() == Some("取消后的请求完成")
        }));
        assert_eq!(
            harness
                .state
                .session_store
                .canonical_turn_for_session_turn_id(&session_id, &first_turn_id)
                .expect("首轮取消事实应保留在历史")
                .status,
            CanonicalTurnStatus::Cancelled
        );
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
    async fn workspace_task_provider_failure_settles_git_lease_before_next_turn() {
        let harness = MagiTurnHarness::new_task("第二轮完成");
        let (workspace_id, workspace_root) = register_git_workspace(&harness);
        let session_id = SessionId::new("harness-provider-failure-lease-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "Provider 失败后继续下一轮",
                Some(workspace_id.to_string()),
            )
            .expect("workspace session should create");
        harness
            .state
            .ensure_session_code_context(&session_id, &Some(workspace_id.clone()))
            .await
            .expect("workspace Git context should initialize");
        harness
            .state
            .release_session_git_execution_lease(&session_id);
        harness.provider.set_failure("第一轮 Provider 故障");

        let first = harness
            .submit_workspace_task(
                &session_id,
                &workspace_id,
                &workspace_root,
                "执行第一轮任务并验证 Provider 失败收口",
                "harness-provider-failure-lease-first",
                "harness-provider-failure-lease-first-user",
            )
            .await
            .expect("第一轮应先被接纳");
        let first_turn_id = first.turn_id.clone().expect("第一轮应有 Turn");
        let first_root_task_id = first.root_task_id.clone().expect("第一轮应有 root task");
        assert_eq!(
            harness
                .wait_for_terminal(&session_id, &first_turn_id)
                .await
                .status,
            CanonicalTurnStatus::Failed
        );
        assert_eq!(
            harness
                .wait_for_task_terminal(&magi_core::TaskId::new(first_root_task_id))
                .await
                .status,
            magi_core::TaskStatus::Failed
        );

        harness.provider.set_completed_response("第二轮完成");
        let second = harness
            .submit_workspace_task(
                &session_id,
                &workspace_id,
                &workspace_root,
                "执行第二轮任务并返回完成结果",
                "harness-provider-failure-lease-second",
                "harness-provider-failure-lease-second-user",
            )
            .await
            .expect("第一轮失败后第二轮仍应能够接纳");
        let second_turn_id = second.turn_id.clone().expect("第二轮应有 Turn");
        let second_turn = harness
            .wait_for_terminal(&session_id, &second_turn_id)
            .await;
        assert_eq!(second_turn.status, CanonicalTurnStatus::Completed);
        assert!(second_turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item.content.as_deref() == Some("第二轮完成")
        }));

        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn workspace_task_provider_failure_releases_turn_slot_but_keeps_explicit_continue_chain()
    {
        let harness = MagiTurnHarness::new_task("继续后完成");
        let (workspace_id, workspace_root) = register_git_workspace(&harness);
        let session_id = SessionId::new("harness-provider-failure-continue-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "Provider 失败后显式继续",
                Some(workspace_id.to_string()),
            )
            .expect("workspace session should create");
        harness
            .state
            .ensure_session_code_context(&session_id, &Some(workspace_id.clone()))
            .await
            .expect("workspace Git context should initialize");
        harness
            .state
            .release_session_git_execution_lease(&session_id);
        harness.provider.set_failure("第一轮 Provider 故障");

        let first = harness
            .submit_workspace_task(
                &session_id,
                &workspace_id,
                &workspace_root,
                "执行第一轮任务并保留可继续的执行链",
                "harness-provider-failure-continue-first",
                "harness-provider-failure-continue-first-user",
            )
            .await
            .expect("第一轮应先被接纳");
        let first_turn_id = first.turn_id.clone().expect("第一轮应有 Turn");
        let first_root_task_id = first.root_task_id.clone().expect("第一轮应有 root task");
        assert_eq!(
            harness
                .wait_for_terminal(&session_id, &first_turn_id)
                .await
                .status,
            CanonicalTurnStatus::Failed
        );
        assert_eq!(
            harness
                .wait_for_task_terminal(&magi_core::TaskId::new(first_root_task_id.clone()))
                .await
                .status,
            magi_core::TaskStatus::Failed
        );
        assert!(
            harness
                .state
                .session_store
                .active_execution_chain(&session_id)
                .is_some(),
            "失败 Turn 的可继续执行链必须保留"
        );
        let mut coordinator_released = false;
        for _ in 0..200 {
            if harness
                .state
                .turn_coordinator()
                .current_attempt(&session_id, &first_turn_id)
                .is_err()
            {
                coordinator_released = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            coordinator_released,
            "失败 Turn 终态完成后不得继续占用 Coordinator 当前槽位"
        );

        harness.provider.set_completed_response("继续后完成");
        let response = crate::routes::build_router(harness.state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/session/continue")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": session_id,
                            "workspaceId": workspace_id,
                            "requestId": "harness-provider-failure-continue-next",
                            "userMessageId": "harness-provider-failure-continue-next-user"
                        })
                        .to_string(),
                    ))
                    .expect("continue request should build"),
            )
            .await
            .expect("continue route should respond");
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("continue response body should read");
        assert_eq!(
            status,
            StatusCode::OK,
            "显式 Continue 应在失败 Turn 收口后被接纳: {}",
            String::from_utf8_lossy(&body)
        );
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("continue response should be JSON");
        let continued_turn_id = payload["turnId"]
            .as_str()
            .expect("continue response should expose turnId")
            .to_string();
        let continued_turn = harness
            .wait_for_terminal(&session_id, &continued_turn_id)
            .await;
        assert_eq!(continued_turn.status, CanonicalTurnStatus::Completed);
        assert!(continued_turn.items.iter().any(|item| {
            item.kind == CanonicalTurnItemKind::AssistantText
                && item.content.as_deref() == Some("继续后完成")
        }));
        assert_eq!(
            harness
                .state
                .session_store
                .canonical_turn_for_session_turn_id(&session_id, &first_turn_id)
                .expect("failed source turn should remain in history")
                .status,
            CanonicalTurnStatus::Failed
        );

        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn transient_provider_failure_is_not_rerun_above_the_http_layer() {
        // 暂态故障的重试只在 HTTP 调用层（带退避、可见进度）发生一次；调用层放弃后，
        // 会话运行时不再悄悄把同一请求重跑一遍，否则一次故障会被放大成成倍的请求。
        let harness = MagiTurnHarness::new("不应出现");
        harness.provider.set_transient_failures_then_completed(
            "不应出现",
            "provider request timed out",
            1,
        );
        let response = harness
            .submit(
                None,
                "暂态 Provider 故障",
                "harness-transient-request",
                "harness-transient-user",
            )
            .await
            .expect("暂态故障仍应先接纳 Turn");
        let session_id = SessionId::new(response.session_id.clone());
        let turn_id = response.turn_id.clone().expect("Turn 应有 ID");
        let turn = harness.wait_for_terminal(&session_id, &turn_id).await;
        assert_eq!(turn.status, CanonicalTurnStatus::Failed);
        assert_eq!(
            harness.provider.requests().len(),
            1,
            "调用层放弃后不应再重跑同一请求"
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
