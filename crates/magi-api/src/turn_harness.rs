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
    task_runner_bridge::TaskCompletionSink,
};
use magi_core::{AccessProfile, SessionId, UtcMillis};
use magi_event_bus::{EventEnvelope, InMemoryEventBus};
use magi_governance::GovernanceService;
use magi_memory_store::MemoryStore;
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{
    ActiveExecutionTurn, CanonicalTurn, CanonicalTurnItemKind, SessionStore, TimelineEntryInput,
    TimelineEntryKind,
};
use magi_tool_runtime::{ExternalMcpToolExecutor, ExternalToolCatalogSnapshot, ToolRegistry};
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
        child_steps: Vec<(String, String)>,
    },
    NestedAgents {
        hold_leaf: bool,
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
    execution_scopes: Vec<magi_bridge_client::ModelExecutionScope>,
    deltas: Vec<ModelStreamingDelta>,
    tool_round_emitted: usize,
    tool_round_limit: usize,
    agent_spawn_emitted: usize,
    repeat_spawn_task_name: bool,
    agent_lifetime: Option<String>,
    resume_agent_task: Option<String>,
    transient_failures_remaining: usize,
    reported_usage: Option<serde_json::Value>,
    before_response: Option<Arc<dyn Fn() + Send + Sync>>,
    agent_cancel_ready: Option<Arc<dyn Fn() + Send + Sync>>,
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
            child_steps: Vec::new(),
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
            child_steps: Vec::new(),
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
        let hook = self.state.lock().unwrap().before_response.clone();
        if !request.prompt.contains("Session Turn 编排分类器")
            && let Some(hook) = hook
        {
            hook();
        }
        let mut response = self.invoke_streaming_unmetered(request, on_delta)?;
        response.usage = self.state.lock().unwrap().reported_usage.clone();
        Ok(response)
    }

    fn invoke_streaming_unmetered(
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
            ProviderBehavior::NestedAgents { hold_leaf } => {
                let tools = request.tools.as_deref().unwrap_or_default();
                let has_tool = |name: &str| tools.iter().any(|tool| tool.function.name == name);
                let mainline = !has_tool("context_request");
                let delegator = has_tool("agent_spawn");
                let messages = request.messages.as_deref().unwrap_or_default();
                let calls = messages
                    .iter()
                    .flat_map(|message| &message.tool_calls)
                    .filter(|call| call.id.starts_with("nested-step-"))
                    .count();
                let child = messages
                    .iter()
                    .filter(|message| message.role == "tool")
                    .filter_map(|message| {
                        serde_json::from_str::<serde_json::Value>(message.content.as_deref()?).ok()
                    })
                    .find_map(|value| value["child_task_id"].as_str().map(str::to_owned));
                let spawn = |name: &str, delegate: bool| {
                    let lifetime = self
                        .state
                        .lock()
                        .unwrap()
                        .agent_lifetime
                        .clone()
                        .unwrap_or_else(|| "turn".into());
                    (
                        "agent_spawn",
                        serde_json::json!({
                            "task_name": name, "display_name": name, "role": "executor", "goal": name,
                            "delegation": {"purpose":"user_requested", "reason":"验收请求明确要求实际父子代理执行与逐级合入"},
                            "allow_delegation": delegate,
                            "lifetime": lifetime,
                            "context_package": {"summary":name, "constraints":[], "expected_output":"文件与验证结果", "references":[]}
                        }),
                    )
                };
                let wait = || {
                    (
                        "agent_wait",
                        serde_json::json!({"task_ids":[child], "timeout_ms":60_000}),
                    )
                };
                let apply = || ("agent_apply", serde_json::json!({"task_id":child}));
                let action = if mainline {
                    match calls {
                        0 => Some(spawn("nested_parent", true)),
                        1 => Some(wait()),
                        2 => Some(apply()),
                        _ => None,
                    }
                } else if delegator {
                    match calls {
                        0 => Some((
                            "file_write",
                            serde_json::json!({"path":"parent.txt", "content":"parent uncommitted"}),
                        )),
                        1 => Some(spawn("nested_leaf", false)),
                        2 => Some(wait()),
                        3 => Some(apply()),
                        4 => Some(("file_read", serde_json::json!({"path":"leaf.txt"}))),
                        _ => None,
                    }
                } else {
                    let job_id = messages
                        .iter()
                        .rev()
                        .filter(|message| message.role == "tool")
                        .filter_map(|message| {
                            serde_json::from_str::<serde_json::Value>(message.content.as_deref()?)
                                .ok()
                        })
                        .find_map(|result| result["job_id"].as_str().map(str::to_owned));
                    match calls {
                        0 => Some(("file_read", serde_json::json!({"path":"parent.txt"}))),
                        1 if hold_leaf => Some((
                            "shell_exec",
                            serde_json::json!({"command":"sleep 30; printf orphan > orphan.txt", "background":true}),
                        )),
                        2 if hold_leaf => Some((
                            "shell_exec",
                            serde_json::json!({"action":"wait", "job_id":job_id, "timeout_ms":60000}),
                        )),
                        1 => Some((
                            "file_write",
                            serde_json::json!({"path":"leaf.txt", "content":"leaf output"}),
                        )),
                        _ => None,
                    }
                };
                if let Some((name, arguments)) = action {
                    let call = ChatToolCall {
                        id: format!("nested-step-{calls}"),
                        kind: "function".into(),
                        function: magi_bridge_client::ChatToolFunction {
                            name: name.into(),
                            arguments: arguments.to_string(),
                        },
                    };
                    self.record_delta(
                        &ModelStreamingDelta {
                            tool_calls: vec![call.clone()],
                            ..Default::default()
                        },
                        on_delta,
                        track_timing,
                    );
                    Ok(ModelResponse {
                        status: ModelResponseStatus::RequiresToolExecution,
                        content: None,
                        thinking: None,
                        tool_calls: vec![call],
                        usage: None,
                        finish_reason: Some("tool_calls".into()),
                        provider_context: Vec::new(),
                    })
                } else {
                    Ok(ModelResponse::completed("嵌套代理完成"))
                }
            }
            ProviderBehavior::AgentSpawnThenWait {
                response,
                child_count,
                role,
                display_name,
                child_steps,
            } => {
                let classifier_request = request.prompt.contains("Session Turn 编排分类器");
                let messages = request.messages.as_deref().unwrap_or_default();
                // Goal 转交夹具沿真实启动合同建立 Goal/计划，再进入代理链路。
                // 保留 forced tool 的真实输入，不通过直接修改 SessionStore 伪造归属。
                if self.state.lock().unwrap().agent_lifetime.as_deref() == Some("goal") {
                    let forced = request
                        .tool_choice
                        .as_ref()
                        .map(|choice| choice.function.name.trim_start_matches("magi_builtin_"));
                    let arguments = match forced {
                        Some("get_goal") => Some(serde_json::json!({})),
                        Some("create_goal") => {
                            Some(serde_json::json!({"objective":"Goal 转交控制验收"}))
                        }
                        Some("update_plan") => {
                            let result = messages
                                .iter()
                                .rev()
                                .filter(|message| message.role == "tool")
                                .filter_map(|message| message.content.as_deref())
                                .filter_map(|text| {
                                    serde_json::from_str::<serde_json::Value>(text).ok()
                                })
                                .find(|result| {
                                    result.get("goal").is_some_and(|goal| goal.is_object())
                                })
                                .expect("计划前应已读取或创建 Goal");
                            Some(serde_json::json!({
                                "planId":result["plan"]["planId"],
                                "expectedRevision":result["plan"]["revision"].as_u64().unwrap_or(0),
                                "expectedGoalId":result["goal"]["goalId"],
                                "expectedGoalControlRevision":result["goal"]["controlRevision"],
                                "language":"zh-CN",
                                "plan":[{"itemId":null,"step":"验证 Goal 代理转交和控制撤销","status":"in_progress"}]
                            }))
                        }
                        _ => None,
                    };
                    if let Some(arguments) = arguments {
                        return Ok(ModelResponse {
                            status: ModelResponseStatus::RequiresToolExecution,
                            content: None,
                            thinking: None,
                            tool_calls: vec![ChatToolCall {
                                id: format!("harness-goal-{}", forced.unwrap()),
                                kind: "function".into(),
                                function: magi_bridge_client::ChatToolFunction {
                                    name: forced.unwrap().into(),
                                    arguments: arguments.to_string(),
                                },
                            }],
                            usage: None,
                            finish_reason: Some("tool_calls".into()),
                            provider_context: Vec::new(),
                        });
                    }
                }
                let current_messages = &messages[messages
                    .iter()
                    .rposition(|message| {
                        // 末尾追加的引用/计划等运行时上下文也是 user 角色，不是回合起点。
                        message.role == "user"
                            && message.context_origin
                                == magi_bridge_client::ChatMessageOrigin::Conversation
                    })
                    .unwrap_or(0)..];
                let spawn_count = current_messages
                    .iter()
                    .flat_map(|message| message.tool_calls.iter())
                    .filter(|call| call.function.name == "agent_spawn")
                    .count();
                let wait_count = current_messages
                    .iter()
                    .flat_map(|message| message.tool_calls.iter())
                    .filter(|call| {
                        matches!(call.function.name.as_str(), "agent_wait" | "agent_cancel")
                    })
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
                let (agent_lifetime, resume_agent_task, repeat_spawn_task_name) = {
                    let state = self.state.lock().expect("harness state");
                    (
                        state.agent_lifetime.clone(),
                        state.resume_agent_task.clone(),
                        state.repeat_spawn_task_name,
                    )
                };
                if can_emit_agent_spawn {
                    let child_index = spawn_count + 1;
                    let task_index = if repeat_spawn_task_name {
                        1
                    } else {
                        child_index
                    };
                    let tool_call = ChatToolCall {
                        id: format!("harness-agent-spawn-call-{child_index}"),
                        kind: "function".to_string(),
                        function: magi_bridge_client::ChatToolFunction {
                            name: "agent_spawn".to_string(),
                            arguments: {
                                let mut arguments = serde_json::json!({
                                    "task_name": format!("harness_child_{task_index}"),
                                    "delegation": {"purpose":"user_requested", "reason":"当前验收请求明确要求派发子代理并收集结果"},
                                    "display_name": format!("{display_name}{task_index}"),
                                    "role": role,
                                    "goal": "完成 harness 子任务并返回明确结果",
                                    "context_package": {
                                        "summary": "harness 子代理上下文",
                                        "constraints": ["只验证子代理链路"],
                                        "expected_output": "子代理完成",
                                        "references": []
                                    }
                                });
                                if let Some(lifetime) = agent_lifetime {
                                    arguments["lifetime"] = lifetime.into();
                                }
                                if let Some(task) = resume_agent_task {
                                    arguments["resume_task_id"] = task.into();
                                }
                                arguments.to_string()
                            },
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
                let wait_pending = current_messages
                    .iter()
                    .rev()
                    .filter(|message| message.role == "tool")
                    .filter_map(|message| message.content.as_deref())
                    .filter_map(|content| serde_json::from_str::<serde_json::Value>(content).ok())
                    .find(|payload| payload["tool"] == "agent_wait")
                    .is_some_and(|payload| payload["all_terminal"] == false);
                if !classifier_request && coordinator_surface && (wait_count == 0 || wait_pending) {
                    let mut child_task_ids = current_messages
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
                    if repeat_spawn_task_name {
                        // 丢掉第一次成功回执，验证仅凭冲突回执也能回到原代理。
                        child_task_ids = current_messages
                            .iter()
                            .filter(|message| message.role == "tool")
                            .filter_map(|message| message.content.as_deref())
                            .filter_map(|content| {
                                serde_json::from_str::<serde_json::Value>(content).ok()
                            })
                            .filter_map(|payload| {
                                payload["existing_task_id"].as_str().map(str::to_string)
                            })
                            .collect();
                    }
                    let expected_children = if repeat_spawn_task_name {
                        1
                    } else {
                        child_count
                    };
                    if child_task_ids.len() >= expected_children {
                        let cancel_ready = self.state.lock().unwrap().agent_cancel_ready.clone();
                        if let Some(ready) = &cancel_ready {
                            ready();
                        }
                        let tool_call = ChatToolCall {
                            id: format!("harness-agent-wait-call-{wait_count}"),
                            kind: "function".to_string(),
                            function: magi_bridge_client::ChatToolFunction {
                                name: if cancel_ready.is_some() { "agent_cancel" } else { "agent_wait" }.into(),
                                arguments: if cancel_ready.is_some() {
                                    serde_json::json!({"task_id": child_task_ids[0], "reason":"工作方向已变更，停止原代理"})
                                } else {
                                    serde_json::json!({"task_ids": child_task_ids, "timeout_ms": 60_000})
                                }.to_string(),
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
                if !classifier_request && !coordinator_surface {
                    let executed = messages
                        .iter()
                        .flat_map(|message| &message.tool_calls)
                        .filter(|call| call.id.starts_with("harness-child-tool-"))
                        .count();
                    if let Some((name, arguments)) = child_steps.get(executed) {
                        let tool_call = ChatToolCall {
                            id: format!("harness-child-tool-{executed}"),
                            kind: "function".into(),
                            function: magi_bridge_client::ChatToolFunction {
                                name: name.clone(),
                                arguments: arguments.clone(),
                            },
                        };
                        self.record_delta(
                            &ModelStreamingDelta {
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
                            finish_reason: Some("tool_calls".into()),
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
                    let arguments = if arguments.contains("$last_job_id") {
                        let job_id = request
                            .messages
                            .as_deref()
                            .unwrap_or_default()
                            .iter()
                            .rev()
                            .filter(|message| message.role == "tool")
                            .filter_map(|message| message.content.as_deref())
                            .filter_map(|content| {
                                serde_json::from_str::<serde_json::Value>(content).ok()
                            })
                            .find_map(|result| {
                                result
                                    .get("job_id")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_string)
                            })
                            .expect("Job 等待测试必须先收到真实启动回执");
                        arguments.replace("$last_job_id", &job_id)
                    } else {
                        arguments
                    };
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
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn bind_execution_scope(
        &self,
        scope: magi_bridge_client::ModelExecutionScope,
    ) -> Result<Option<Arc<dyn ModelBridgeClient>>, BridgeClientError> {
        self.state.lock().unwrap().execution_scopes.push(scope);
        Ok(None)
    }

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
                let released_content = {
                    let state = self
                        .state
                        .lock()
                        .expect("harness provider state should hold");
                    match state.behavior.as_ref() {
                        Some(ProviderBehavior::Completed(content)) => Some(content.clone()),
                        _ => None,
                    }
                };
                if let Some(content) = released_content {
                    self.record_delta(
                        &ModelStreamingDelta {
                            content: content.clone(),
                            ..Default::default()
                        },
                        on_delta,
                        true,
                    );
                    return Ok(ModelResponse::completed(content));
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
    runtime_directory: Arc<tempfile::TempDir>,
}

impl Drop for MagiTurnHarness {
    fn drop(&mut self) {
        // 断言失败或测试有意留下运行中的调用时，先撤下交互和任务，避免 Tokio
        // shutdown 等待一个再也收不到用户答复的 blocking worker。
        if let Some(store) = self.state.task_store() {
            let roots = store
                .all_tasks()
                .into_iter()
                .filter(|task| {
                    task.parent_task_id.is_none()
                        && matches!(
                            task.status,
                            magi_core::TaskStatus::Pending | magi_core::TaskStatus::Running
                        )
                })
                .map(|task| task.task_id)
                .collect::<Vec<_>>();
            if let Some(manager) = self.state.runner_manager() {
                for root in roots {
                    let _ = manager.kill_tree(root.as_str());
                }
            }
        }
        for session in self.state.session_store.sessions() {
            self.state
                .turn_coordinator()
                .tool_approvals()
                .remove_session(&session.session_id);
        }
    }
}

#[derive(Default)]
struct HarnessExecutionOptions {
    settings: Option<Arc<magi_settings_store::SettingsStore>>,
    skill_runtime: Option<Arc<magi_skill_runtime::SkillRuntime>>,
    persist_session_state:
        Option<Arc<magi_conversation_runtime::session_writeback::SessionStatePersistCallback>>,
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
        Self::from_parts_with_external_mcp(
            session_store,
            provider,
            with_task_runtime,
            None,
            None,
            HarnessExecutionOptions::default(),
        )
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
            HarnessExecutionOptions::default(),
        )
    }

    fn from_parts_with_external_mcp(
        session_store: Arc<SessionStore>,
        provider: HarnessModelClient,
        with_task_runtime: bool,
        external_mcp_catalog: Option<ExternalToolCatalogSnapshot>,
        external_mcp_executor: Option<ExternalMcpToolExecutor>,
        options: HarnessExecutionOptions,
    ) -> Self {
        let HarnessExecutionOptions {
            settings,
            skill_runtime,
            persist_session_state,
        } = options;
        let event_bus = Arc::new(InMemoryEventBus::new(512));
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let git_service = Arc::new(magi_git::GitService::new());
        let session_code_contexts = magi_git::SessionCodeContextRegistry::default();
        let workspace_git_coordinator = magi_git::WorkspaceGitOperationCoordinator::default();
        let knowledge_store = Arc::new(magi_knowledge_store::KnowledgeStore::new());
        let runtime_directory = Arc::new(
            tempfile::Builder::new()
                .prefix("magi-turn-harness-")
                .tempdir()
                .expect("Turn harness runtime directory should create"),
        );
        let runtime_state_root = runtime_directory.path();
        let snapshot_manager = Arc::new(crate::managed_snapshot_manager(
            runtime_state_root.to_path_buf(),
            session_store.clone(),
        ));
        let runtime_persistence = Arc::new(RuntimeStatePersistence::new(
            runtime_state_root.to_path_buf(),
            runtime_state_root.join("workspaces.json"),
            runtime_state_root.join("knowledge.json"),
        ));
        // 普通 Conversation harness 不创建 TaskStore；这与生产 profile 边界一致，
        // 不能只是不把已经创建的 TaskStore 挂到 ApiState 上。
        let task_store = with_task_runtime.then(|| Arc::new(TaskStore::new()));
        let git_tool_executor = crate::git_tool_runtime::build_git_tool_executor(
            crate::git_tool_runtime::GitToolRuntimeDependencies {
                task_store: task_store.clone(),
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
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus))
            .with_background_job_journal(
                session_store.background_job_journal(runtime_state_root.join("job-output")),
            )
            .with_git_tool_executor(git_tool_executor);
        if let Some(catalog) = external_mcp_catalog.clone() {
            tool_registry = tool_registry
                .with_external_tool_catalog_provider(Arc::new(move || catalog.clone()));
        }
        if let Some(executor) = external_mcp_executor.clone() {
            tool_registry = tool_registry.with_external_mcp_tool_executor(executor);
        }
        tool_registry.register_default_builtins();
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
        if let Some(settings) = settings.as_ref() {
            state = state.with_settings_store(settings.clone());
        }
        if let Some(runtime) = skill_runtime.as_ref() {
            state = state.with_skill_runtime(runtime.clone());
        }
        if let Some(task_store) = task_store.as_ref() {
            state = state.with_task_store(Arc::clone(task_store));
            state
                .task_execution_registry()
                .clone()
                .with_agent_spawn_preflight_runtime(AgentSpawnPreflightRuntime {
                    settings_store: settings.clone(),
                    default_model_client: Some(Arc::new(provider.clone())),
                    session_code_contexts: Some(session_code_contexts.clone()),
                    git_service_configured: true,
                    skill_runtime: skill_runtime.clone(),
                    ..AgentSpawnPreflightRuntime::default()
                });
        }
        let completion_sink: Arc<dyn TaskCompletionSink> = match completion_notifier.as_ref() {
            Some(notifier) => notifier.clone(),
            None => Arc::new(|_| -> () { panic!("无任务运行时不能派发 Task 终态") }),
        };
        let mut dispatcher_builder = LlmTaskDispatcher::new(
            Arc::clone(&event_bus),
            ExecutionPipeline {
                task_store: task_store
                    .clone()
                    .unwrap_or_else(|| Arc::new(TaskStore::new())),
                memory_store: MemoryStore::new(),
            },
            LlmTaskDispatcherDependencies {
                session_store: Arc::clone(&session_store),
                execution_registry: state.task_execution_registry().clone(),
                completion_sink,
                conversation_registry: Arc::clone(&state.conversation_registry),
                agent_role_registry: Arc::clone(&state.agent_role_registry),
            },
            runtime_state_root.join("missions"),
        )
        .with_model_bridge_client(Arc::new(provider.clone()))
        .with_workspace_registry(Arc::clone(&state.workspace_registry))
        .with_git_context_runtime(git_service, session_code_contexts)
        .with_workspace_git_coordinator(workspace_git_coordinator)
        .with_snapshot_manager(snapshot_manager)
        .with_workspace_execution_preparer(
            crate::task_dispatch::workspace_execution_preparer(&state),
        );
        if let Some(settings) = settings {
            dispatcher_builder = dispatcher_builder.with_settings_store(settings);
        }
        if let Some(runtime) = skill_runtime {
            dispatcher_builder = dispatcher_builder.with_skill_runtime(runtime);
        }
        if let Some(persist) = persist_session_state {
            dispatcher_builder = dispatcher_builder.with_session_state_persist_callback(persist);
        }
        if let Some(notifier) = completion_notifier.as_ref() {
            dispatcher_builder = dispatcher_builder.with_completion_notifier(notifier.clone());
        }
        let dispatcher = Arc::new(dispatcher_builder.with_tool_registry(tool_registry));
        state = state
            .with_session_turn_dispatcher(Arc::clone(&dispatcher))
            .with_model_bridge_client(Arc::new(provider.clone()));
        if let Some(notifier) = completion_notifier {
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
                crate::task_turn_finalize::handle_task_completion_notification(
                    &completion_state,
                    notification,
                );
            });
        }
        let harness = Self {
            state,
            provider,
            external_mcp_catalog,
            external_mcp_executor,
            runtime_directory,
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
            HarnessExecutionOptions {
                skill_runtime: self.state.skill_runtime.clone(),
                ..Default::default()
            },
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
                plugin_command_id: None,
                source_folder_id: None,
                project_directories: None,
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
                interrupt_turn_id: None,
                command: None,
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
                plugin_command_id: None,
                source_folder_id: None,
                project_directories: None,
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
                interrupt_turn_id: None,
                command: None,
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
                plugin_command_id: None,
                source_folder_id: None,
                project_directories: None,
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
                interrupt_turn_id: None,
                command: None,
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
                plugin_command_id: None,
                source_folder_id: None,
                project_directories: None,
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
                interrupt_turn_id: None,
                command: None,
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
                plugin_command_id: None,
                source_folder_id: None,
                project_directories: None,
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
                interrupt_turn_id: None,
                command: None,
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
        crate::turn_service::interrupt_session_turn(
            &self.state,
            session_id,
            workspace_id.map(|id| id.as_str()),
            None,
        )
        .await
        .map(|_| ())
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
        "Git harness fixture command {:?} at {} failed ({}): stdout={}; stderr={}",
        args,
        path.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
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
    let root = harness
        .runtime_directory
        .path()
        .join(format!("git-workspace-{sequence}"));
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
mod tests;
