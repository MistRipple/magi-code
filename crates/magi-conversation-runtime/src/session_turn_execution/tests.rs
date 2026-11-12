use super::*;

#[test]
fn image_only_steer_is_delivered_with_its_input_identity_and_original_source() {
    let image = crate::session_images::SessionTurnImage::from_data_url(
        "image.png",
        "data:image/png;base64,AA==",
    )
    .unwrap();
    let mut messages = Vec::new();
    append_session_turn_steers_to_messages(
        &mut messages,
        vec![UserSignal {
            text: None,
            images: vec![image.source.clone()],
            request_id: Some("image-steer".into()),
            user_message_id: Some("user-image-steer".into()),
            placeholder_message_id: None,
            accepted_at: magi_core::UtcMillis(1),
        }],
    );
    assert_eq!(messages[0].input_request_id.as_deref(), Some("image-steer"));
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.is_none());
    assert_eq!(messages[0].images, vec![image.source]);
}
use crate::{
    CanonicalTurnEventSink, CoordinatorAdmission, CoordinatorCommandResult, CoordinatorTurnStatus,
    ExecutionProfile, SessionTurnCoordinator, TurnAdmission, TurnAttempt, TurnCommand,
};
use magi_bridge_client::{
    BridgeClientError, BridgeErrorLayer, ModelResponse, ModelRetryRuntimeEvent,
    ModelRetryRuntimePhase,
};
use magi_core::{MissionId, SessionLifecycleStatus, TaskId};
use magi_session_store::{
    ActiveExecutionChain, ActiveExecutionDispatchContext, ActiveExecutionTurn, CanonicalToolCall,
    CanonicalTurn, CanonicalTurnItem, CanonicalTurnItemKind, CanonicalTurnItemStatus,
    CanonicalTurnStatus, CanonicalTurnVisibility, CanonicalWorkerRef, ExecutionThread,
    ExecutionThreadStatus, ORCHESTRATOR_ROLE_ID, SessionRecord, SessionStoreState,
    ThreadChatMessage, ThreadChatToolCall, ThreadChatToolFunction, TimelineEntry,
    TimelineEntryInput, TimelineEntryKind,
};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    time::Duration,
};

fn model_response(payload: serde_json::Value) -> ModelResponse {
    ModelResponse::from_chat_payload(
        serde_json::from_value(payload).expect("测试模型响应必须符合统一响应结构"),
    )
}

fn ts(value: u64) -> UtcMillis {
    UtcMillis(value)
}

#[allow(clippy::too_many_arguments)]
fn seed_task_turn(
    store: &SessionStore,
    session_id: &SessionId,
    turn_id: &str,
    turn_seq: u64,
    accepted_at: UtcMillis,
    user_message: &str,
    thread_id: magi_core::ThreadId,
    coordinator: &SessionTurnCoordinator,
) -> TurnAttempt {
    let request_id = format!("request-{turn_id}");
    let request_fingerprint = format!("fingerprint-{turn_id}");
    let attempt = match coordinator
        .execute_command(
            session_id,
            TurnCommand::Start(TurnAdmission {
                turn_id: turn_id.to_string(),
                request_id: request_id.clone(),
                request_fingerprint: request_fingerprint.clone(),
                profile: ExecutionProfile::Task,
            }),
        )
        .expect("测试 Turn 应先经过 Coordinator 接纳")
    {
        CoordinatorCommandResult::Admission(CoordinatorAdmission::Accepted(attempt)) => attempt,
        other => panic!("unexpected fixture admission: {other:?}"),
    };
    let mut item = session_turn_item(
        "user_message",
        "completed",
        None,
        Some(user_message.to_string()),
        Some(format!("user-{turn_id}")),
        thread_id,
    );
    item.source = "user".to_string();
    item.item_seq = 1;
    item.request_id = Some(request_id.clone());
    item.metadata
        .insert("executionProfile".to_string(), serde_json::json!("task"));
    item.metadata
        .insert("requestId".to_string(), serde_json::json!(request_id));
    item.metadata.insert(
        "requestFingerprint".to_string(),
        serde_json::json!(request_fingerprint),
    );
    item.metadata.insert(
        "attemptId".to_string(),
        serde_json::json!(attempt.attempt_id.clone()),
    );
    let task_id = TaskId::new(turn_id.to_string());
    let mission_id = MissionId::new(format!("mission-{session_id}"));
    let task = magi_core::Task {
        task_id: task_id.clone(),
        mission_id: mission_id.clone(),
        root_task_id: task_id.clone(),
        parent_task_id: None,
        kind: magi_core::TaskKind::LocalAgent,
        title: user_message.to_string(),
        goal: user_message.to_string(),
        status: magi_core::TaskStatus::Running,
        dependency_ids: Vec::new(),
        required_children: Vec::new(),
        policy_snapshot: None,
        executor_binding: None,
        completion_contract: Default::default(),
        recovery_checkpoint: None,
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: None,
        input_refs: Vec::new(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        runtime_payload: Default::default(),
        created_at: accepted_at,
        updated_at: accepted_at,
    };
    let admitted_turn = ActiveExecutionTurn {
        turn_id: turn_id.to_string(),
        turn_seq,
        accepted_at,
        completed_at: None,
        status: "accepted".to_string(),
        user_message: Some(user_message.to_string()),
        items: vec![item],
    };
    CanonicalTurnEventSink::for_store(store, None)
        .accept_active_execution_chain_with_timeline_entry_and_task(
            session_id.clone(),
            TimelineEntryInput::new(
                format!("timeline-{turn_id}"),
                TimelineEntryKind::UserMessage,
                user_message,
                accepted_at,
            ),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id,
                root_task_id: task_id,
                execution_chain_ref: format!("chain-{turn_id}"),
                workspace_id: None,
                branches: Vec::new(),
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at,
                    entry_id: format!("timeline-{turn_id}"),
                    trimmed_text: Some(user_message.to_string()),
                    skill_name: None,
                },
                current_turn_id: Some(admitted_turn.turn_id.clone()),
            },
            admitted_turn,
            &task,
        )
        .expect("测试 Turn 应通过 canonical sink 持久化");
    coordinator
        .execute_command(
            session_id,
            TurnCommand::SetStatus {
                attempt: attempt.clone(),
                status: CoordinatorTurnStatus::Preparing,
            },
        )
        .expect("测试 Turn 应进入 preparing");
    coordinator
        .execute_command(
            session_id,
            TurnCommand::SetStatus {
                attempt: attempt.clone(),
                status: CoordinatorTurnStatus::Running,
            },
        )
        .expect("测试 Turn 应进入 running");
    CanonicalTurnEventSink::for_store(store, None)
        .set_status_domain(session_id, Some(turn_id), "running")
        .expect("测试 Turn running 状态应通过 canonical sink 持久化");
    attempt
}

fn seed_conversation_turn(
    store: &SessionStore,
    session_id: &SessionId,
    coordinator: &SessionTurnCoordinator,
    mut turn: ActiveExecutionTurn,
) {
    let turn_id = turn.turn_id.clone();
    let request_id = format!("request-{turn_id}");
    let request_fingerprint = format!("fingerprint-{turn_id}");
    let attempt = match coordinator
        .execute_command(
            session_id,
            TurnCommand::Start(TurnAdmission {
                turn_id: turn_id.clone(),
                request_id: request_id.clone(),
                request_fingerprint: request_fingerprint.clone(),
                profile: ExecutionProfile::Conversation,
            }),
        )
        .expect("会话 Turn 应先经过 Coordinator 接纳")
    {
        CoordinatorCommandResult::Admission(CoordinatorAdmission::Accepted(attempt)) => attempt,
        other => panic!("unexpected conversation fixture admission: {other:?}"),
    };
    if let Some(item) = turn
        .items
        .iter_mut()
        .find(|item| item.kind == "user_message")
    {
        item.request_id = Some(request_id.clone());
        item.metadata.insert(
            "executionProfile".to_string(),
            serde_json::json!("conversation"),
        );
        item.metadata
            .insert("requestId".to_string(), serde_json::json!(request_id));
        item.metadata.insert(
            "requestFingerprint".to_string(),
            serde_json::json!(request_fingerprint),
        );
        item.metadata.insert(
            "attemptId".to_string(),
            serde_json::json!(attempt.attempt_id.clone()),
        );
    }
    let user_message = turn.user_message.clone().unwrap_or_default();
    let accepted_at = turn.accepted_at;
    turn.status = "accepted".to_string();
    CanonicalTurnEventSink::for_store(store, None)
        .accept_conversation_turn_with_timeline_entry(
            session_id.clone(),
            None,
            TimelineEntryInput::new(
                format!("timeline-{turn_id}"),
                TimelineEntryKind::UserMessage,
                user_message,
                accepted_at,
            ),
            turn,
        )
        .expect("会话 Turn 应通过 canonical sink 持久化");
    coordinator
        .execute_command(
            session_id,
            TurnCommand::SetStatus {
                attempt: attempt.clone(),
                status: CoordinatorTurnStatus::Preparing,
            },
        )
        .expect("会话 Turn 应进入 preparing");
    coordinator
        .execute_command(
            session_id,
            TurnCommand::SetStatus {
                attempt,
                status: CoordinatorTurnStatus::Running,
            },
        )
        .expect("会话 Turn 应进入 running");
    CanonicalTurnEventSink::for_store(store, None)
        .set_status_domain(session_id, Some(turn_id.as_str()), "running")
        .expect("会话 Turn running 状态应通过 canonical sink 持久化");
}

fn spawn_vision_http_stub() -> (String, mpsc::Receiver<serde_json::Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("识图模型测试服务必须能监听");
    let address = listener.local_addr().expect("识图模型测试地址必须存在");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("识图模型测试服务必须收到请求");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("识图模型测试读取超时必须可设置");
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).expect("识图模型测试请求必须可读取");
            assert!(read > 0, "识图模型测试请求不得提前结束");
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let headers = String::from_utf8(buffer[..header_end].to_vec())
            .expect("识图模型测试请求头必须是 UTF-8");
        assert!(
            headers.starts_with("POST /v1/chat/completions HTTP/1.1"),
            "识图模型必须使用已配置的 OpenAI Chat 协议"
        );
        let content_length = headers
            .split("\r\n")
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .expect("识图模型测试请求必须包含 Content-Length");
        while buffer.len() < header_end + content_length {
            let read = stream
                .read(&mut chunk)
                .expect("识图模型测试请求体必须可读取");
            assert!(read > 0, "识图模型测试请求体不得提前结束");
            buffer.extend_from_slice(&chunk[..read]);
        }
        let payload = serde_json::from_slice::<serde_json::Value>(
            &buffer[header_end..header_end + content_length],
        )
        .expect("识图模型测试请求体必须是 JSON");
        sender.send(payload).expect("识图模型测试请求必须可回传");
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"识图模型完成\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body,
        );
        stream
            .write_all(response.as_bytes())
            .expect("识图模型测试响应必须可写入");
        stream.flush().expect("识图模型测试响应必须可刷新");
    });
    (format!("http://{address}/v1"), receiver)
}

struct StreamingTextModelBridgeClient {
    delta_content: String,
    payload: String,
}

struct CountingEmptyModelBridgeClient {
    calls: AtomicUsize,
}

struct CountingTextModelBridgeClient {
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelInvocationRequest>>,
}

struct EmptyStreamThenRecoveredSessionModelBridgeClient {
    streaming_calls: AtomicUsize,
}

struct RetryEventModelBridgeClient;

struct SemanticContextCompactionModelBridgeClient {
    requests: Mutex<Vec<ModelInvocationRequest>>,
}

struct FailingContextCompactionModelBridgeClient {
    calls: AtomicUsize,
}

struct CancellingContextCompactionModelBridgeClient {
    store: Arc<SessionStore>,
    session_id: SessionId,
}

struct CancellingModelBridgeClient {
    store: Arc<SessionStore>,
    session_id: SessionId,
}

impl ModelBridgeClient for CancellingModelBridgeClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, _request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        panic!("session turn should use cancellable streaming invocation")
    }

    fn invoke_streaming(
        &self,
        _request: ModelInvocationRequest,
        _on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        panic!("session turn should use cancellable streaming invocation")
    }

    fn invoke_streaming_with_cancellation(
        &self,
        _request: ModelInvocationRequest,
        _on_delta: &dyn Fn(&ModelStreamingDelta),
        _on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        CanonicalTurnEventSink::for_store(&self.store, None)
            .cancel_turn(&self.session_id)
            .expect("turn cancellation should succeed");
        assert!(is_cancelled());
        Err(magi_bridge_client::model_invocation_cancelled_error())
    }
}

impl ModelBridgeClient for RetryEventModelBridgeClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, _request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        Ok(model_response(
            serde_json::json!({ "content": "重连后完成" }),
        ))
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        on_delta(&ModelStreamingDelta {
            content: "重连后完成".to_string(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            ..Default::default()
        });
        self.invoke(request)
    }

    fn invoke_streaming_with_retry_events(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
    ) -> Result<ModelResponse, BridgeClientError> {
        on_retry(&ModelRetryRuntimeEvent {
            phase: ModelRetryRuntimePhase::Scheduled,
            attempt: 1,
            max_attempts: 5,
            delay_ms: Some(10_000),
        });
        on_retry(&ModelRetryRuntimeEvent {
            phase: ModelRetryRuntimePhase::AttemptStarted,
            attempt: 1,
            max_attempts: 5,
            delay_ms: None,
        });
        let response = self.invoke_streaming(request, on_delta);
        on_retry(&ModelRetryRuntimeEvent {
            phase: ModelRetryRuntimePhase::Settled,
            attempt: 1,
            max_attempts: 5,
            delay_ms: None,
        });
        response
    }
}

impl ModelBridgeClient for StreamingTextModelBridgeClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, _request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        Ok(model_response(
            serde_json::from_str(&self.payload).expect("测试模型响应必须是 JSON"),
        ))
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        on_delta(&ModelStreamingDelta {
            content: self.delta_content.clone(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            ..Default::default()
        });
        self.invoke(request)
    }
}

impl ModelBridgeClient for CountingEmptyModelBridgeClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, _request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(model_response(serde_json::json!({
            "content": null,
            "finish_reason": "stop"
        })))
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        _on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke(request)
    }
}

impl ModelBridgeClient for CountingTextModelBridgeClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .expect("主模型请求记录锁必须可用")
            .push(request);
        Ok(model_response(serde_json::json!({
            "content": "主模型完成",
            "finish_reason": "stop"
        })))
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        on_delta(&ModelStreamingDelta {
            content: "主模型完成".to_string(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            ..Default::default()
        });
        self.invoke(request)
    }
}

impl ModelBridgeClient for SemanticContextCompactionModelBridgeClient {
    fn invoke_with_cancellation_and_output_limit(
        &self,
        request: ModelInvocationRequest,
        limit: u32,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        assert!(limit > 0, "压缩夹具必须收到实际输出上限");
        self.invoke_with_cancellation(request, cancelled)
    }

    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        self.requests
            .lock()
            .expect("context compaction requests mutex poisoned")
            .push(request);
        Ok(ModelResponse::completed(
            "## 目标与完成标准\n- 完成当前任务。\n## 约束与权限\n- 遵守当前访问模式。\n## 工作区事实\n- facts.txt 内容哈希保持有效。\n## 工具与外部操作\n- file_read 已成功读取。\n## 代理状态\n- 无。\n## 已确认决策\n- 继续当前方案。\n## 阻塞与风险\n- 无。\n## 下一步\n- 继续当前任务。\n## 禁止重复\n- 不重复读取已确认文件。",
        ))
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        _on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke(request)
    }
}

impl ModelBridgeClient for FailingContextCompactionModelBridgeClient {
    fn invoke_with_cancellation_and_output_limit(
        &self,
        request: ModelInvocationRequest,
        limit: u32,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        assert!(limit > 0, "压缩夹具必须收到实际输出上限");
        self.invoke_with_cancellation(request, cancelled)
    }

    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, _request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            code: Some(503),
            message: "compaction unavailable".to_string(),
        })
    }

    fn invoke_streaming(
        &self,
        _request: ModelInvocationRequest,
        _on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        panic!("上下文压缩不应调用流式接口")
    }
}

impl ModelBridgeClient for CancellingContextCompactionModelBridgeClient {
    fn invoke_with_cancellation_and_output_limit(
        &self,
        request: ModelInvocationRequest,
        limit: u32,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        assert!(limit > 0, "压缩夹具必须收到实际输出上限");
        self.invoke_with_cancellation(request, cancelled)
    }

    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, _request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        CanonicalTurnEventSink::for_store(&self.store, None)
            .cancel_turn(&self.session_id)
            .expect("turn cancellation should succeed");
        Ok(ModelResponse::completed("不应安装的压缩摘要"))
    }

    fn invoke_streaming(
        &self,
        _request: ModelInvocationRequest,
        _on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        panic!("上下文压缩不应调用流式接口")
    }
}

impl ModelBridgeClient for EmptyStreamThenRecoveredSessionModelBridgeClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, _request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        Ok(model_response(serde_json::json!({
            "content": "主线在暂态空流后完成。",
            "finish_reason": "stop"
        })))
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        let attempt = self.streaming_calls.fetch_add(1, Ordering::SeqCst);
        if attempt < 2 {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32007),
                message: "provider response invalid: empty stream response".to_string(),
            });
        }
        let controls = request
            .messages
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|message| {
                message.content.as_deref() == Some(model_empty_response_recovery_prompt(false))
            })
            .collect::<Vec<_>>();
        assert_eq!(controls.len(), 1, "重复恢复只能保留当前控制提示");
        assert_eq!(
            controls[0].context_origin,
            magi_bridge_client::ChatMessageOrigin::RoundContext
        );
        on_delta(&ModelStreamingDelta {
            content: "主线在暂态空流后完成。".to_string(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            ..Default::default()
        });
        self.invoke(request)
    }
}

#[test]
fn model_identity_prompt_distinguishes_model_from_magi() {
    let prompt = model_identity_prompt_for_request("当前模型是什么？", "grok-4");

    let prompt = prompt.expect("模型身份问题应生成身份规则");
    assert!(prompt.contains("grok-4"));
    assert!(prompt.contains("Magi 解析后实际发起请求的配置目标"));
    assert!(prompt.contains("不要把 Magi 冒充成模型"));
    assert!(!prompt.contains("Magi 是承载当前对话的 AI 工作台"));
}

#[test]
fn model_identity_prompt_describes_magi_only_for_tool_questions() {
    let prompt = model_identity_prompt_for_request("当前工具是什么？", "grok-4");

    let prompt = prompt.expect("工具身份问题应生成身份规则");
    assert!(prompt.contains("Magi 是承载当前对话的 AI 工作台"));
    assert!(!prompt.contains("当前请求目标模型为“grok-4”"));
}

#[test]
fn model_identity_prompt_is_not_added_to_regular_questions() {
    assert!(model_identity_prompt_for_request("读取 README.md", "grok-4").is_none());
}

struct RecordingHooks {
    events: std::sync::Mutex<Vec<(&'static str, serde_json::Value)>>,
}
impl crate::extension_hooks::ExtensionHooks for RecordingHooks {
    fn tool_call(
        &self,
        _: &crate::extension_hooks::ToolHookScope<'_>,
        _: &magi_bridge_client::ChatToolCall,
    ) -> crate::extension_hooks::ToolCallDecision {
        unreachable!("纯会话没有工具")
    }
    fn tool_result(
        &self,
        _: &crate::extension_hooks::ToolHookScope<'_>,
        _: &magi_bridge_client::ChatToolCall,
        _: crate::extension_hooks::ToolResultView<'_>,
    ) -> Option<String> {
        None
    }
    fn observe(&self, hook: &'static str, payload: serde_json::Value) {
        self.events.lock().unwrap().push((hook, payload));
    }
    fn prompt_contribute(
        &self,
        _: &crate::extension_hooks::ToolHookScope<'_>,
        view: &crate::extension_hooks::ContributeView<'_>,
    ) -> Vec<crate::extension_hooks::PromptContribution> {
        use crate::extension_hooks::{ContributePhase, ContributionStability, PromptContribution};
        let (id, stability, text) = match view.phase {
            ContributePhase::Start => (
                "house-style",
                ContributionStability::Stable,
                "回答使用简体中文。",
            ),
            ContributePhase::Round => ("round-note", ContributionStability::Volatile, "本轮提醒"),
        };
        vec![PromptContribution {
            plugin_id: "acme.style".into(),
            package_digest: "feedfacefeedface".into(),
            fragment_id: id.into(),
            stability,
            priority: 0,
            text: text.into(),
        }]
    }
}

struct CaptureAndAnswerClient {
    sent: std::sync::Mutex<Vec<Vec<ChatMessage>>>,
}
impl ModelBridgeClient for CaptureAndAnswerClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }
    fn invoke(&self, request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        self.sent
            .lock()
            .unwrap()
            .push(request.messages.clone().unwrap_or_default());
        Ok(model_response(serde_json::json!({
            "content": "已回答", "finish_reason": "stop"
        })))
    }
    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        on_delta(&ModelStreamingDelta {
            content: "已回答".to_string(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            ..Default::default()
        });
        self.invoke(request)
    }
}

#[test]
fn conversation_profile_applies_contributions_and_notifies_model_observers() {
    let session_id = SessionId::new("session-conversation-hooks");
    let turn_id = "turn-conversation-hooks".to_string();
    let store = Arc::new(SessionStore::new());
    store.create_session(session_id.clone(), "hooks").unwrap();
    let (_mission, thread) = store.ensure_session_mission(&session_id, ts(900), || {
        magi_core::MissionId::new("mission-conversation-hooks")
    });
    let coordinator = Arc::new(SessionTurnCoordinator::new());
    seed_task_turn(
        store.as_ref(),
        &session_id,
        &turn_id,
        1,
        ts(1_000),
        "你好",
        thread,
        coordinator.as_ref(),
    );
    let registry = ConversationRegistry::with_turn_coordinator(coordinator);
    registry
        .turn_coordinator()
        .begin_session_turn_input(session_id.clone(), turn_id.clone())
        .unwrap();
    let client = CaptureAndAnswerClient {
        sent: Default::default(),
    };
    let hooks = RecordingHooks {
        events: Default::default(),
    };
    let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
    let output = run_session_turn_execution_for_test(SessionTurnExecutionRuntime {
        client: &client,
        event_bus: &InMemoryEventBus::new(16),
        session_store: store.as_ref(),
        conversation_registry: &registry,
        plan_store: &plan_store,
        settings_store: None,
        safety_gate: None,
        skill_runtime: None,
        request: SessionTurnExecutionRequest {
            project_directories: None,
            session_id,
            turn_id,
            workspace_id: None,
            prompt: "你好".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            command: None,
        },
        prompt: "你好".to_string(),
        knowledge_context_prompt: None,
        persist_session_state: None,
        live_settings_store: None,
        extension_hooks: Some(&hooks),
    })
    .expect("会话轮次应完成");
    assert!(!output.interrupted);
    let sent = client.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    let messages = &sent[0];
    let find = |needle: &str| {
        messages
            .iter()
            .position(|m| m.content.as_deref().is_some_and(|c| c.contains(needle)))
    };
    let stable = find("stability=\"stable\"").expect("稳定贡献在请求里");
    let volatile = find("stability=\"volatile\"").expect("易变贡献在请求里");
    let user = messages
        .iter()
        .position(|m| m.content.as_deref() == Some("你好"))
        .expect("当前输入");
    assert!(
        stable < user && user < volatile,
        "稳定在前缀，易变在当前输入之后"
    );
    assert_eq!(messages[stable].role, "user");
    assert_eq!(
        messages[stable].context_origin,
        magi_bridge_client::ChatMessageOrigin::ContextReference
    );
    assert_eq!(
        messages[volatile].context_origin,
        magi_bridge_client::ChatMessageOrigin::RoundContext
    );
    // 模型观察钩子在 Step 提交后依次通知，且不带提示词正文。
    let events = hooks.events.lock().unwrap();
    let names: Vec<_> = events.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, ["model.resolved", "model.request", "model.response"]);
    assert!(
        events
            .iter()
            .all(|(_, payload)| !payload.to_string().contains("你好"))
    );
}

struct RewritingHooks;
impl crate::extension_hooks::ExtensionHooks for RewritingHooks {
    fn tool_call(
        &self,
        _: &crate::extension_hooks::ToolHookScope<'_>,
        _: &magi_bridge_client::ChatToolCall,
    ) -> crate::extension_hooks::ToolCallDecision {
        unreachable!()
    }
    fn tool_result(
        &self,
        _: &crate::extension_hooks::ToolHookScope<'_>,
        _: &magi_bridge_client::ChatToolCall,
        _: crate::extension_hooks::ToolResultView<'_>,
    ) -> Option<String> {
        None
    }
    fn context_assemble(
        &self,
        _: &crate::extension_hooks::ToolHookScope<'_>,
        view: &crate::extension_hooks::AssembleView<'_>,
    ) -> Vec<crate::ExtensionRewriteProposal> {
        vec![crate::ExtensionRewriteProposal {
            plugin_id: "acme.summary".into(),
            contribution_id: "context.assemble".into(),
            hook: "context.assemble".into(),
            from: view.rewritable.start,
            to: view.rewritable.start + 2,
            replacement: vec![ChatMessage {
                input_request_id: None,
                context_origin: Default::default(),
                role: "assistant".into(),
                content: Some("[摘要]".into()),
                images: vec![],
                tool_calls: vec![],
                tool_call_id: None,
                provider_context: vec![],
            }],
        }]
    }
    fn context_rewrite_authorized(&self, plugin_id: &str) -> bool {
        plugin_id == "acme.summary"
    }
}

#[test]
fn conversation_assembly_rewrites_history_records_it_and_keeps_the_tail() {
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: SessionId::new("s"),
        turn_id: "t".into(),
        workspace_id: None,
        prompt: "当前".into(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };
    let msg = |role: &str, text: &str| ChatMessage {
        input_request_id: None,
        context_origin: Default::default(),
        role: role.into(),
        content: Some(text.into()),
        images: vec![],
        tool_calls: vec![],
        tool_call_id: None,
        provider_context: vec![],
    };
    let history = vec![
        msg("user", "u0"),
        msg("assistant", "a0"),
        msg("user", "u1"),
        msg("assistant", "a1"),
        msg("user", "当前"),
    ];
    let assembly = ConversationAssembly::new(Some(&RewritingHooks), &request, 100_000);
    let view = assembly.view(
        &history,
        &crate::request_controls::RequestControls::default(),
    );
    assert_eq!(
        view.iter()
            .map(|m| m.content.clone().unwrap())
            .collect::<Vec<_>>(),
        ["[摘要]", "u1", "a1", "当前"]
    );
    let projection = assembly.projection.borrow().clone();
    assert_eq!(projection.extension_rewrites.len(), 1);
    assert_eq!(projection.extension_rewrites[0].plugin_id, "acme.summary");
    // 没有扩展时视图与原控制视图一致。
    let plain = ConversationAssembly::new(None, &request, 100_000).view(
        &history,
        &crate::request_controls::RequestControls::default(),
    );
    assert_eq!(plain.len(), history.len());
}

#[test]
fn conversation_projects_historical_tools_and_rechecks_disabled_plugin() {
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: SessionId::new("projection-session"),
        turn_id: "projection-turn".into(),
        workspace_id: None,
        prompt: "continue".into(),
        images: vec![],
        context_references: vec![],
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };
    let raw = crate::test_history::tool_projection_history("historical")
        .iter()
        .map(thread_chat_message_to_chat_message)
        .collect::<Vec<_>>();
    let hooks = crate::test_history::ToolProjectionHooks::new();
    let controls = crate::request_controls::RequestControls::default();
    let first = ConversationAssembly::new(Some(&hooks), &request, 128_000);
    let view = first.view(&raw, &controls);
    assert_eq!(view[1].content.as_deref(), Some("[projection-v1]"));
    hooks.version.store(0, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(first.view(&raw, &controls)[1].content, view[1].content);
    let next = ConversationAssembly::new(Some(&hooks), &request, 128_000);
    assert!(
        next.view(&raw, &controls)[1]
            .content
            .as_deref()
            .unwrap()
            .contains("SOURCE_SECRET")
    );
}

#[test]
fn session_turn_cancellation_interrupts_model_invocation() {
    let session_id = SessionId::new("session-model-cancellation");
    let turn_id = "turn-model-cancellation".to_string();
    let store = Arc::new(SessionStore::new());
    store
        .create_session(session_id.clone(), "model cancellation")
        .expect("session should be creatable");
    let (_mission_id, orchestrator_thread_id) =
        store.ensure_session_mission(&session_id, ts(900), || {
            magi_core::MissionId::new("mission-model-cancellation")
        });
    let coordinator = Arc::new(SessionTurnCoordinator::new());
    seed_task_turn(
        store.as_ref(),
        &session_id,
        &turn_id,
        1,
        ts(1_000),
        "停止测试",
        orchestrator_thread_id,
        coordinator.as_ref(),
    );
    let registry = ConversationRegistry::with_turn_coordinator(coordinator);
    registry
        .turn_coordinator()
        .begin_session_turn_input(session_id.clone(), turn_id.clone())
        .expect("turn input should begin");
    let client = CancellingModelBridgeClient {
        store: store.clone(),
        session_id: session_id.clone(),
    };
    let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
    let output = run_session_turn_execution_for_test(SessionTurnExecutionRuntime {
        client: &client,
        event_bus: &InMemoryEventBus::new(16),
        session_store: store.as_ref(),
        conversation_registry: &registry,
        plan_store: &plan_store,
        settings_store: None,
        safety_gate: None,
        skill_runtime: None,
        request: SessionTurnExecutionRequest {
            project_directories: None,
            session_id,
            turn_id,
            workspace_id: None,
            prompt: "停止测试".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            command: None,
        },
        prompt: "停止测试".to_string(),
        knowledge_context_prompt: None,
        persist_session_state: None,
        live_settings_store: None,
        extension_hooks: None,
    })
    .expect("cancelled model invocation should resolve as interrupted turn");
    assert!(output.interrupted);
}

#[test]
fn interrupted_canonical_turn_is_not_writable() {
    assert!(!current_turn_status_is_writable("interrupted"));
    assert!(!current_turn_status_is_writable("cancelled"));
    assert!(current_turn_status_is_writable("running"));
}

#[test]
fn vision_model_only_takes_over_the_current_image_turn() {
    let (vision_base_url, vision_request_receiver) = spawn_vision_http_stub();
    let session_id = SessionId::new("session-vision-takeover");
    let turn_id = "turn-vision-takeover".to_string();
    let store = Arc::new(SessionStore::new());
    store
        .create_session(session_id.clone(), "vision takeover")
        .expect("识图接管测试会话必须可创建");
    let (_, thread_id) = store.ensure_session_mission(&session_id, ts(900), || {
        magi_core::MissionId::new("mission-vision-takeover")
    });
    let mut persisted = store.export_state();
    persisted.canonical_turns.push(
        serde_json::from_value(serde_json::json!({
            "sessionId": session_id, "turnId": "vision-previous-turn", "turnSeq": 1,
            "acceptedAt": 950, "completedAt": 950, "status": "completed",
            "items": [
                {"sessionId": session_id, "turnId": "vision-previous-turn", "turnSeq": 1,
                 "itemId": "vision-previous-user", "itemSeq": 1, "kind": "user_message",
                 "createdAt": 950, "updatedAt": 950, "status": "completed",
                 "sourceThreadId": thread_id, "content": "历史约束：保留表格结构"},
                {"sessionId": session_id, "turnId": "vision-previous-turn", "turnSeq": 1,
                 "itemId": "vision-previous-answer", "itemSeq": 2, "kind": "assistant_text",
                 "createdAt": 950, "updatedAt": 950, "status": "completed",
                 "sourceThreadId": thread_id, "content": "已确认历史约束"}
            ],
        }))
        .unwrap(),
    );
    let store = Arc::new(SessionStore::from_state_for_test(persisted));
    let coordinator = SessionTurnCoordinator::new();
    seed_conversation_turn(
        store.as_ref(),
        &session_id,
        &coordinator,
        ActiveExecutionTurn {
            turn_id: turn_id.clone(),
            turn_seq: 1,
            accepted_at: ts(1_000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("结合图片继续处理".to_string()),
            items: vec![session_turn_item(
                "user_message",
                "completed",
                None,
                Some("结合图片继续处理".to_string()),
                Some("user-vision-takeover".to_string()),
                thread_id.clone(),
            )],
        },
    );
    let settings = Arc::new(SettingsStore::new());
    settings
        .set_section(
            "orchestrator",
            crate::main_model_sources::test_sources(
                "https://main.example.com/v1",
                "main-key",
                &["deepseek-v4-flash"],
            ),
        )
        .expect("主模型数据源必须可写入");
    settings
            .set_section(
                magi_settings_store::ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
                serde_json::json!({"sourceId": "custom/test", "model": "deepseek-v4-flash", "reasoningEffort": "medium"}),
            )
            .expect("主模型默认配置必须可写入");
    settings
        .set_section(
            crate::model_config::VISION_MODEL_SECTION,
            serde_json::json!({
                "baseUrl": vision_base_url,
                "apiKey": "vision-key",
                "model": "vision-model",
                "apiProtocol": "openai_chat",
                "contextWindowTokens": 128000,
                "textModelRules": []
            }),
        )
        .expect("识图模型配置必须可写入");
    let registry = ConversationRegistry::with_turn_coordinator(Arc::new(coordinator));
    registry
        .turn_coordinator()
        .begin_session_turn_input(session_id.clone(), turn_id.clone())
        .expect("识图接管测试输入边界必须可创建");
    let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
    let main_client = CountingTextModelBridgeClient {
        calls: AtomicUsize::new(0),
        requests: Mutex::new(Vec::new()),
    };
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id,
        workspace_id: None,
        prompt: "结合图片继续处理".to_string(),
        images: vec![
            SessionTurnImage::from_data_url("table.png", "data:image/png;base64,AAA")
                .expect("识图接管测试图片必须有效"),
        ],
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: Some("request-vision-takeover".to_string()),
        user_message_id: Some("user-vision-takeover".to_string()),
        placeholder_message_id: Some("assistant-vision-takeover".to_string()),
        command: None,
    };

    let output = run_session_turn_execution_for_test(SessionTurnExecutionRuntime {
        client: &main_client,
        event_bus: &InMemoryEventBus::new(32),
        session_store: store.as_ref(),
        conversation_registry: &registry,
        plan_store: &plan_store,
        settings_store: Some(&settings),
        safety_gate: None,
        skill_runtime: None,
        request,
        prompt: "结合图片继续处理".to_string(),
        knowledge_context_prompt: None,
        persist_session_state: None,
        live_settings_store: Some(settings.clone()),
        extension_hooks: None,
    })
    .expect("识图模型必须完成整个会话轮次");

    assert_eq!(output.final_content, "识图模型完成");
    assert_eq!(main_client.calls.load(Ordering::SeqCst), 0);
    let payload = vision_request_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("识图模型必须且只需收到一次完整请求");
    assert_eq!(payload["model"], "vision-model");
    let serialized = payload.to_string();
    assert!(serialized.contains("历史约束：保留表格结构"));
    assert!(serialized.contains("已确认历史约束"));
    assert!(serialized.contains("结合图片继续处理"));
    assert!(serialized.contains("data:image/png;base64,AAA"));
    assert!(
        vision_request_receiver.try_recv().is_err(),
        "识图接管不得先识图再调用第二次模型"
    );

    let persisted_image_turn = store
        .thread_context_snapshot(&thread_id)
        .unwrap()
        .messages()
        .to_vec();
    assert!(
        persisted_image_turn
            .iter()
            .filter(|message| message.content.as_deref() == Some("结合图片继续处理"))
            .all(|message| message.images.is_empty()),
        "图片只属于当前回合输入，不能进入后续回合的模型历史"
    );

    let follow_up_turn_id = "turn-after-vision-takeover".to_string();
    let follow_up_coordinator = SessionTurnCoordinator::new();
    seed_conversation_turn(
        store.as_ref(),
        &session_id,
        &follow_up_coordinator,
        ActiveExecutionTurn {
            turn_id: follow_up_turn_id.clone(),
            turn_seq: 2,
            accepted_at: ts(2_000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("继续说明刚才的结论".to_string()),
            items: vec![session_turn_item(
                "user_message",
                "completed",
                None,
                Some("继续说明刚才的结论".to_string()),
                Some("user-after-vision-takeover".to_string()),
                thread_id.clone(),
            )],
        },
    );
    let follow_up_registry =
        ConversationRegistry::with_turn_coordinator(Arc::new(follow_up_coordinator));
    follow_up_registry
        .turn_coordinator()
        .begin_session_turn_input(session_id.clone(), follow_up_turn_id.clone())
        .expect("识图后的纯文本输入边界必须可创建");
    let follow_up_request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: follow_up_turn_id,
        workspace_id: None,
        prompt: "继续说明刚才的结论".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: Some("request-after-vision-takeover".to_string()),
        user_message_id: Some("user-after-vision-takeover".to_string()),
        placeholder_message_id: Some("assistant-after-vision-takeover".to_string()),
        command: None,
    };
    let follow_up_output = run_session_turn_execution_for_test(SessionTurnExecutionRuntime {
        client: &main_client,
        event_bus: &InMemoryEventBus::new(32),
        session_store: store.as_ref(),
        conversation_registry: &follow_up_registry,
        plan_store: &plan_store,
        settings_store: Some(&settings),
        safety_gate: None,
        skill_runtime: None,
        request: follow_up_request,
        prompt: "继续说明刚才的结论".to_string(),
        knowledge_context_prompt: None,
        persist_session_state: None,
        live_settings_store: Some(settings.clone()),
        extension_hooks: None,
    })
    .expect("识图后的纯文本轮次必须恢复主模型执行");

    assert_eq!(follow_up_output.final_content, "主模型完成");
    assert_eq!(main_client.calls.load(Ordering::SeqCst), 1);
    let main_requests = main_client
        .requests
        .lock()
        .expect("主模型请求记录锁必须可用");
    assert_eq!(main_requests.len(), 1);
    assert!(
        main_requests[0]
            .messages
            .as_ref()
            .expect("主模型后续轮次必须携带完整文本上下文")
            .iter()
            .all(|message| message.images.is_empty()),
        "后续纯文本轮次不得向主模型重放历史图片"
    );
}

struct FailingModelBridgeClient {
    message: String,
}

impl ModelBridgeClient for FailingModelBridgeClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, _request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(-32007),
            message: self.message.clone(),
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

struct StreamingThenFailingModelBridgeClient {
    delta_content: String,
    message: String,
}

struct InterruptedThenRecoveredSessionModelBridgeClient {
    streaming_calls: AtomicUsize,
    non_stream_calls: AtomicUsize,
    recovery_messages: Mutex<Vec<ChatMessage>>,
}

struct SteeringModelBridgeClient {
    store: SessionStore,
    registry: Arc<ConversationRegistry>,
    session_id: SessionId,
    turn_id: String,
    calls: AtomicUsize,
    requests: std::sync::Mutex<Vec<ModelInvocationRequest>>,
}

struct PlanFollowUpModelBridgeClient {
    plan_store: magi_plan::PlanStore,
    calls: AtomicUsize,
    requests: std::sync::Mutex<Vec<ModelInvocationRequest>>,
}

impl ModelBridgeClient for SteeringModelBridgeClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .expect("request log lock")
            .push(request);
        if call == 0 {
            let thread = self
                .store
                .orchestrator_thread_for_session(&self.session_id)
                .unwrap();
            let mut item = session_turn_item(
                "user_message",
                "completed",
                None,
                Some("优先收口，不要继续扩展".into()),
                Some("user-steer-runtime".into()),
                thread.thread_id,
            );
            item.request_id = Some("request-steer-runtime".into());
            item.user_message_id = Some("user-steer-runtime".into());
            item.metadata.insert(
                "requestFingerprint".into(),
                serde_json::json!("steer-fingerprint"),
            );
            item.metadata
                .insert("route".into(), serde_json::json!("steer"));
            self.registry
                .turn_coordinator()
                .try_steer_session_turn_with(
                    &self.session_id,
                    &self.turn_id,
                    UserSignal {
                        images: Vec::new(),
                        text: Some("优先收口，不要继续扩展".to_string()),
                        request_id: Some("request-steer-runtime".to_string()),
                        user_message_id: Some("user-steer-runtime".to_string()),
                        placeholder_message_id: None,
                        accepted_at: ts(1_100),
                    },
                    || {
                        self.store.upsert_current_turn_item_for_turn(
                            &self.session_id,
                            Some(&self.turn_id),
                            item,
                        )
                    },
                )
                .expect("steer should be accepted while first model call is active");
        }
        let content = if call == 0 {
            "第一段回复"
        } else {
            "最终收口"
        };
        let mut response = model_response(serde_json::json!({
            "content": content,
            "finish_reason": "stop"
        }));
        if call == 0 {
            response.provider_context = vec![ModelProviderContext {
                provider: "anthropic".to_string(),
                kind: "thinking".to_string(),
                data: serde_json::json!({
                    "type": "thinking",
                    "thinking": "先响应第一阶段",
                    "signature": "session-signed-thinking"
                }),
            }];
        }
        Ok(response)
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        let next_call = self.calls.load(Ordering::SeqCst);
        on_delta(&ModelStreamingDelta {
            content: if next_call == 0 {
                "第一段回复".to_string()
            } else {
                "最终收口".to_string()
            },
            thinking: String::new(),
            tool_calls: Vec::new(),
            ..Default::default()
        });
        self.invoke(request)
    }
}

impl ModelBridgeClient for PlanFollowUpModelBridgeClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .expect("request log lock")
            .push(request);
        if call == 39 {
            let current = self.plan_store.snapshot().expect("plan should exist");
            self.plan_store
                .update(magi_plan::UpdatePlanInput {
                    plan_id: Some(current.plan_id.to_string()),
                    expected_revision: Some(current.revision),
                    expected_goal_id: None,
                    expected_goal_control_revision: None,
                    language: current.language,
                    explanation: None,
                    plan: current
                        .items
                        .into_iter()
                        .map(|item| magi_plan::UpdatePlanItemInput {
                            item_id: Some(item.item_id.to_string()),
                            step: item.title,
                            status: magi_core::PlanItemStatus::Completed,
                        })
                        .collect(),
                })
                .expect("final model round should complete plan");
        }
        Ok(model_response(serde_json::json!({
            "content": if call < 39 { "继续推进当前计划" } else { "全部计划完成" },
            "finish_reason": "stop"
        })))
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        let call = self.calls.load(Ordering::SeqCst);
        on_delta(&ModelStreamingDelta {
            content: if call < 39 {
                "继续推进当前计划".to_string()
            } else {
                "全部计划完成".to_string()
            },
            thinking: String::new(),
            tool_calls: Vec::new(),
            ..Default::default()
        });
        self.invoke(request)
    }
}

#[test]
fn active_turn_steer_continues_same_turn_with_a_second_model_call() {
    let session_id = SessionId::new("session-runtime-steer");
    let turn_id = "turn-runtime-steer".to_string();
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "runtime steer")
        .expect("session should be creatable");
    let (_mission_id, orchestrator_thread_id) =
        store.ensure_session_mission(&session_id, ts(900), || {
            magi_core::MissionId::new("mission-runtime-steer")
        });
    let registry = Arc::new(ConversationRegistry::new());
    seed_conversation_turn(
        &store,
        &session_id,
        registry.turn_coordinator(),
        ActiveExecutionTurn {
            turn_id: turn_id.clone(),
            turn_seq: 1_000,
            accepted_at: ts(1_000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("请给出完整方案".to_string()),
            items: vec![session_turn_item(
                "user_message",
                "completed",
                None,
                Some("请给出完整方案".to_string()),
                Some("user-runtime-steer".to_string()),
                orchestrator_thread_id.clone(),
            )],
        },
    );
    registry
        .turn_coordinator()
        .begin_session_turn_input(session_id.clone(), turn_id.clone())
        .expect("turn input should begin");
    let client = SteeringModelBridgeClient {
        store: store.clone(),
        registry: registry.clone(),
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        calls: AtomicUsize::new(0),
        requests: std::sync::Mutex::new(Vec::new()),
    };
    let event_bus = InMemoryEventBus::new(32);
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id,
        workspace_id: None,
        prompt: "请给出完整方案".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: Some("request-runtime-steer".to_string()),
        user_message_id: Some("user-runtime-steer".to_string()),
        placeholder_message_id: Some("assistant-runtime-steer".to_string()),
        command: None,
    };

    let output = run_session_turn_execution_for_test(SessionTurnExecutionRuntime {
        client: &client,
        event_bus: &event_bus,
        session_store: &store,
        conversation_registry: registry.as_ref(),
        plan_store: &crate::test_plan_store("test-plan"),
        settings_store: None,
        safety_gate: None,
        skill_runtime: None,
        request,
        prompt: "请给出完整方案".to_string(),
        knowledge_context_prompt: None,
        persist_session_state: None,
        live_settings_store: None,
        extension_hooks: None,
    })
    .expect("steered turn should complete");

    assert_eq!(output.final_content, "最终收口");
    let requests = client.requests.lock().expect("request log lock");
    assert_eq!(requests.len(), 2);
    let second_messages = requests[1]
        .messages
        .as_ref()
        .expect("second call should carry messages");
    assert!(second_messages.iter().any(|message| {
        message.role == "assistant"
            && message.content.as_deref() == Some("第一段回复")
            && message
                .provider_context
                .first()
                .is_some_and(|context| context.data["signature"] == "session-signed-thinking")
    }));
    assert!(second_messages.iter().any(|message| {
        message.role == "user" && message.content.as_deref() == Some("优先收口，不要继续扩展")
    }));
    assert!(
        store
            .thread_context_snapshot(&orchestrator_thread_id)
            .unwrap()
            .messages()
            .iter()
            .any(|message| {
                message.role == "assistant"
                    && message.content.as_deref() == Some("第一段回复")
                    && message.provider_context.first().is_some_and(|context| {
                        context.data["signature"] == "session-signed-thinking"
                    })
            }),
        "主会话 thread 必须持久化提供方签名上下文"
    );
}

#[test]
fn ordinary_turn_does_not_consume_waiting_goal_plan() {
    let session_id = SessionId::new("session-waiting-goal-plan-isolation");
    let goal_turn_id = "turn-goal-owner".to_string();
    let ordinary_turn_id = "turn-ordinary-diversion".to_string();
    let store = Arc::new(SessionStore::new());
    store
        .create_session(session_id.clone(), "waiting goal plan isolation")
        .expect("session should be creatable");
    let (_mission_id, orchestrator_thread_id) =
        store.ensure_session_mission(&session_id, ts(900), || {
            magi_core::MissionId::new("mission-waiting-goal-plan-isolation")
        });
    let goal = store
        .create_goal(
            session_id.clone(),
            orchestrator_thread_id.clone(),
            goal_turn_id,
            "验证普通 Turn 不接管等待中的 Goal",
            AccessProfile::Restricted,
        )
        .expect("goal should create");
    let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
    let plan = plan_store
        .update(magi_plan::UpdatePlanInput {
            plan_id: None,
            expected_revision: Some(0),
            expected_goal_id: Some(goal.goal_id.to_string()),
            expected_goal_control_revision: Some(goal.control_revision),
            language: "zh-CN".to_string(),
            explanation: None,
            plan: vec![magi_plan::UpdatePlanItemInput {
                item_id: Some("goal-step".to_string()),
                step: "继续 Goal".to_string(),
                status: magi_core::PlanItemStatus::InProgress,
            }],
        })
        .expect("goal plan should create");
    let paused = store
        .pause_goal_with_plan(
            &session_id,
            &goal.goal_id,
            goal.control_revision,
            Some(plan.revision),
        )
        .expect("goal should pause")
        .0;
    let paused_plan = store.plan(&session_id).expect("paused plan should exist");
    store
        .resume_goal_with_plan(
            &session_id,
            &goal.goal_id,
            paused.control_revision,
            Some(paused_plan.revision),
            None,
        )
        .expect("goal resume request should wait for an owner");
    let registry = ConversationRegistry::new();
    seed_conversation_turn(
        &store,
        &session_id,
        registry.turn_coordinator(),
        ActiveExecutionTurn {
            turn_id: ordinary_turn_id.clone(),
            turn_seq: 2,
            accepted_at: ts(1_000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("执行普通任务".to_string()),
            items: vec![session_turn_item(
                "user_message",
                "completed",
                None,
                Some("执行普通任务".to_string()),
                Some("user-ordinary-diversion".to_string()),
                orchestrator_thread_id,
            )],
        },
    );
    registry
        .turn_coordinator()
        .begin_session_turn_input(session_id.clone(), ordinary_turn_id.clone())
        .expect("ordinary turn input should begin");
    let client = PlanFollowUpModelBridgeClient {
        plan_store: plan_store.clone(),
        calls: AtomicUsize::new(0),
        requests: std::sync::Mutex::new(Vec::new()),
    };

    let output = run_session_turn_execution_for_test(SessionTurnExecutionRuntime {
        client: &client,
        event_bus: &InMemoryEventBus::new(16),
        session_store: store.as_ref(),
        conversation_registry: &registry,
        plan_store: &plan_store,
        settings_store: None,
        safety_gate: None,
        skill_runtime: None,
        request: SessionTurnExecutionRequest {
            project_directories: None,
            session_id,
            turn_id: ordinary_turn_id,
            workspace_id: None,
            prompt: "执行普通任务".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            command: None,
        },
        prompt: "执行普通任务".to_string(),
        knowledge_context_prompt: None,
        persist_session_state: None,
        live_settings_store: None,
        extension_hooks: None,
    })
    .expect("ordinary turn should complete independently");

    assert_eq!(output.final_content, "继续推进当前计划");
    assert_eq!(client.calls.load(Ordering::SeqCst), 1);
    assert!(plan_store.requires_execution_follow_up());
    let requests = client.requests.lock().expect("request log lock");
    let messages = requests[0]
        .messages
        .as_ref()
        .expect("ordinary request should include messages");
    assert!(!messages.iter().any(|message| {
        message.content.as_deref().is_some_and(|content| {
            content.contains("当前持久化执行状态") || content.contains("当前执行计划仍未完成")
        })
    }));
}

impl ModelBridgeClient for StreamingThenFailingModelBridgeClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, _request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(-32007),
            message: self.message.clone(),
        })
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        on_delta(&ModelStreamingDelta {
            content: self.delta_content.clone(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            ..Default::default()
        });
        self.invoke(request)
    }
}

impl ModelBridgeClient for InterruptedThenRecoveredSessionModelBridgeClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        self.non_stream_calls.fetch_add(1, Ordering::SeqCst);
        *self
            .recovery_messages
            .lock()
            .expect("recovery messages mutex poisoned") = request.messages.unwrap_or_default();
        Ok(model_response(serde_json::json!({
            "content": "已通过非流式降级完成。",
            "finish_reason": "stop"
        })))
    }

    fn invoke_streaming(
        &self,
        _request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        let attempt = self.streaming_calls.fetch_add(1, Ordering::SeqCst);
        on_delta(&ModelStreamingDelta {
            content: if attempt == 0 {
                "已保留的半截回复".to_string()
            } else {
                "，后续流仍然中断".to_string()
            },
            thinking: String::new(),
            tool_calls: Vec::new(),
            ..Default::default()
        });
        Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            code: Some(-32005),
            message: "provider stream interrupted: missing terminal SSE event".to_string(),
        })
    }
}

#[test]
fn runtime_invalid_state_pauses_active_plan() {
    let session_id = SessionId::new("session-runtime-invalid-state");
    let store = Arc::new(SessionStore::new());
    store
        .create_session(session_id.clone(), "runtime invalid state")
        .expect("session should be creatable");
    let conversation_registry = Arc::new(ConversationRegistry::new());
    seed_conversation_turn(
        store.as_ref(),
        &session_id,
        conversation_registry.turn_coordinator(),
        ActiveExecutionTurn {
            turn_id: "turn-runtime-invalid-state".to_string(),
            turn_seq: 1,
            accepted_at: ts(1_000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("继续处理".to_string()),
            items: Vec::new(),
        },
    );
    let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
    plan_store
        .update(magi_plan::UpdatePlanInput {
            plan_id: None,
            expected_revision: Some(0),
            expected_goal_id: None,
            expected_goal_control_revision: None,
            language: "zh-CN".to_string(),
            explanation: None,
            plan: vec![magi_plan::UpdatePlanItemInput {
                item_id: Some("continue-current-step".to_string()),
                step: "继续处理当前步骤".to_string(),
                status: magi_core::PlanItemStatus::InProgress,
            }],
        })
        .expect("plan should write");
    let client = StreamingTextModelBridgeClient {
        delta_content: "不会执行".to_string(),
        payload: serde_json::json!({
            "content": "不会执行",
            "finish_reason": "stop"
        })
        .to_string(),
    };

    let result = run_session_turn_execution_for_test(SessionTurnExecutionRuntime {
        client: &client,
        event_bus: &InMemoryEventBus::new(16),
        session_store: store.as_ref(),
        conversation_registry: conversation_registry.as_ref(),
        plan_store: &plan_store,
        settings_store: None,
        safety_gate: None,
        skill_runtime: None,
        request: SessionTurnExecutionRequest {
            project_directories: None,
            session_id: session_id.clone(),
            turn_id: "turn-runtime-invalid-state".to_string(),
            workspace_id: None,
            prompt: "继续处理".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            command: None,
        },
        prompt: "继续处理".to_string(),
        knowledge_context_prompt: None,
        persist_session_state: None,
        live_settings_store: None,
        extension_hooks: None,
    });

    assert!(matches!(
        result,
        Err(SessionTurnExecutionError {
            reason: SessionTurnFailureReason::RuntimeInvalidState,
            ..
        })
    ));
    let plan = plan_store.snapshot().expect("plan should remain visible");
    assert_eq!(plan.state, magi_core::PlanState::Paused);
    assert_eq!(plan.items[0].status, magi_core::PlanItemStatus::InProgress);
}

#[test]
fn empty_session_turn_response_uses_public_failure_message() {
    let session_id = SessionId::new("session-empty-response-layer");
    let store = Arc::new(SessionStore::new());
    store
        .create_session(session_id.clone(), "empty response layer")
        .expect("session should be creatable");
    let (_mission_id, orchestrator_thread_id) =
        store.ensure_session_mission(&session_id, ts(910), || {
            magi_core::MissionId::new("mission-empty-response-layer")
        });
    let conversation_registry = Arc::new(ConversationRegistry::new());
    seed_conversation_turn(
        store.as_ref(),
        &session_id,
        conversation_registry.turn_coordinator(),
        ActiveExecutionTurn {
            turn_id: "turn-empty-response-layer".to_string(),
            turn_seq: 1,
            accepted_at: ts(1_000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("请回复一句话".to_string()),
            items: vec![session_turn_item(
                "user_message",
                "completed",
                None,
                Some("请回复一句话".to_string()),
                Some("user-empty-response-layer".to_string()),
                orchestrator_thread_id,
            )],
        },
    );
    let client = CountingEmptyModelBridgeClient {
        calls: AtomicUsize::new(0),
    };
    let event_bus = InMemoryEventBus::new(16);
    let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
    plan_store
        .update(magi_plan::UpdatePlanInput {
            plan_id: None,
            expected_revision: Some(0),
            expected_goal_id: None,
            expected_goal_control_revision: None,
            language: "zh-CN".to_string(),
            explanation: None,
            plan: vec![magi_plan::UpdatePlanItemInput {
                item_id: Some("continue-current-step".to_string()),
                step: "继续处理当前步骤".to_string(),
                status: magi_core::PlanItemStatus::InProgress,
            }],
        })
        .expect("plan should write");
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: "turn-empty-response-layer".to_string(),
        workspace_id: None,
        prompt: "请回复一句话".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };

    let error = match run_session_turn_execution_for_test(SessionTurnExecutionRuntime {
        client: &client,
        event_bus: &event_bus,
        session_store: store.as_ref(),
        conversation_registry: conversation_registry.as_ref(),
        plan_store: &plan_store,
        settings_store: None,
        safety_gate: None,
        skill_runtime: None,
        request,
        prompt: "请回复一句话".to_string(),
        knowledge_context_prompt: None,
        persist_session_state: None,
        live_settings_store: None,
        extension_hooks: None,
    }) {
        Ok(_) => panic!("empty provider response should fail"),
        Err(error) => error,
    };

    assert_eq!(error.reason, SessionTurnFailureReason::ModelEmptyResponse);
    assert_eq!(
        error.public_message,
        "模型服务返回了空响应，未生成正文或可执行工具调用。"
    );
    assert_eq!(
        client.calls.load(Ordering::SeqCst),
        MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS + 1,
        "空响应必须先执行每轮带恢复提示的自动重试，再进入失败终态"
    );
    let turn = store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .expect("failed turn should remain visible");
    assert_eq!(turn.status, "failed");
    let error_item = turn
        .items
        .iter()
        .find(|item| item.kind == "assistant_error")
        .expect("empty response should append an assistant error");
    assert_eq!(
        error_item.content.as_deref(),
        Some(error.public_message.as_str())
    );
    assert_eq!(
        error_item.metadata["modelFailure"]["code"],
        "model_empty_response"
    );
    assert_eq!(
        error_item.metadata["modelFailure"]["retryAttempts"],
        MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS
    );
    assert!(
        error_item.metadata["modelFailure"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("仍未返回用户可见正文"))
    );
    let plan = plan_store.snapshot().expect("plan should remain visible");
    assert_eq!(plan.state, magi_core::PlanState::Paused);
    assert_eq!(plan.items[0].status, magi_core::PlanItemStatus::InProgress);
}

#[test]
fn session_turn_retries_empty_stream_before_output() {
    let session_id = SessionId::new("session-empty-stream-recovery");
    let store = Arc::new(SessionStore::new());
    store
        .create_session(session_id.clone(), "empty stream recovery")
        .expect("session should be creatable");
    let (_mission_id, orchestrator_thread_id) =
        store.ensure_session_mission(&session_id, ts(1_100), || {
            magi_core::MissionId::new("mission-empty-stream-recovery")
        });
    let conversation_registry = Arc::new(ConversationRegistry::new());
    seed_conversation_turn(
        store.as_ref(),
        &session_id,
        conversation_registry.turn_coordinator(),
        ActiveExecutionTurn {
            turn_id: "turn-empty-stream-recovery".to_string(),
            turn_seq: 1,
            accepted_at: ts(1_200),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("请回复一句话".to_string()),
            items: vec![session_turn_item(
                "user_message",
                "completed",
                None,
                Some("请回复一句话".to_string()),
                Some("user-empty-stream-recovery".to_string()),
                orchestrator_thread_id,
            )],
        },
    );
    let client = EmptyStreamThenRecoveredSessionModelBridgeClient {
        streaming_calls: AtomicUsize::new(0),
    };
    let event_bus = InMemoryEventBus::new(16);
    let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: "turn-empty-stream-recovery".to_string(),
        workspace_id: None,
        prompt: "请回复一句话".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };

    let output = run_session_turn_execution_for_test(SessionTurnExecutionRuntime {
        client: &client,
        event_bus: &event_bus,
        session_store: store.as_ref(),
        conversation_registry: conversation_registry.as_ref(),
        plan_store: &plan_store,
        settings_store: None,
        safety_gate: None,
        skill_runtime: None,
        request,
        prompt: "请回复一句话".to_string(),
        knowledge_context_prompt: None,
        persist_session_state: None,
        live_settings_store: None,
        extension_hooks: None,
    })
    .expect("empty stream before output should be retried");

    assert_eq!(output.final_content, "主线在暂态空流后完成。");
    assert_eq!(
        client.streaming_calls.load(Ordering::SeqCst),
        3,
        "主线机械重试仍为空流时，应追加用户可见答复约束后恢复"
    );
    let turn = store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .expect("completed turn should remain visible");
    assert_eq!(turn.status, "completed");
    assert!(turn.items.iter().all(|item| item.kind != "assistant_error"));
}

#[test]
fn partial_stream_failure_preserves_output_before_terminal_recovery_failure() {
    let session_id = SessionId::new("session-partial-stream-failure");
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "partial stream failure")
        .expect("session should be creatable");
    let (_mission_id, orchestrator_thread_id) =
        store.ensure_session_mission(&session_id, ts(915), || {
            magi_core::MissionId::new("mission-partial-stream-failure")
        });
    let conversation_registry = Arc::new(ConversationRegistry::new());
    seed_conversation_turn(
        &store,
        &session_id,
        conversation_registry.turn_coordinator(),
        ActiveExecutionTurn {
            turn_id: "turn-partial-stream-failure".to_string(),
            turn_seq: 1,
            accepted_at: ts(1_000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("请输出长回复".to_string()),
            items: vec![session_turn_item(
                "user_message",
                "completed",
                None,
                Some("请输出长回复".to_string()),
                Some("user-partial-stream-failure".to_string()),
                orchestrator_thread_id,
            )],
        },
    );
    let client = StreamingThenFailingModelBridgeClient {
        delta_content: "这是一段半截输出".to_string(),
        message:
            "provider response invalid: incomplete stream response: missing terminal SSE event"
                .to_string(),
    };
    let event_bus = InMemoryEventBus::new(16);
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: "turn-partial-stream-failure".to_string(),
        workspace_id: None,
        prompt: "请输出长回复".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };

    let error = match run_session_turn_execution_for_test(SessionTurnExecutionRuntime {
        client: &client,
        event_bus: &event_bus,
        session_store: &store,
        conversation_registry: conversation_registry.as_ref(),
        plan_store: &crate::test_plan_store("test-plan"),
        settings_store: None,
        safety_gate: None,
        skill_runtime: None,
        request,
        prompt: "请输出长回复".to_string(),
        knowledge_context_prompt: None,
        persist_session_state: None,
        live_settings_store: None,
        extension_hooks: None,
    }) {
        Ok(_) => panic!("incomplete stream should fail the turn"),
        Err(error) => error,
    };

    assert_eq!(
        error.reason,
        SessionTurnFailureReason::ModelStreamInterrupted
    );
    assert_eq!(error.public_message, "模型响应流在完成前中断。");
    let turn = store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .expect("failed turn should remain visible");
    assert_eq!(turn.status, "failed");
    let error_item = turn
        .items
        .iter()
        .find(|item| item.kind == "assistant_error")
        .expect("stream failure should append an assistant error");
    assert_eq!(
        error_item.content.as_deref(),
        Some(error.public_message.as_str())
    );
    assert_eq!(
        error_item.metadata["modelFailure"]["code"],
        "model_stream_interrupted"
    );
    assert!(
        error_item.metadata["modelFailure"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("missing terminal SSE event"))
    );
    assert!(
        turn.items.iter().any(|item| {
            item.kind == "assistant_stream"
                && item.status == "completed"
                && item.content.as_deref() == Some("这是一段半截输出")
        }),
        "流中断时必须保留已经展示给用户的内容"
    );
}

#[test]
fn session_turn_recovers_after_repeated_partial_stream_interruptions() {
    let session_id = SessionId::new("session-stream-recovery");
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "stream recovery")
        .expect("session should be creatable");
    let (_mission_id, orchestrator_thread_id) =
        store.ensure_session_mission(&session_id, ts(1_500), || {
            magi_core::MissionId::new("mission-stream-recovery")
        });
    let conversation_registry = Arc::new(ConversationRegistry::new());
    seed_conversation_turn(
        &store,
        &session_id,
        conversation_registry.turn_coordinator(),
        ActiveExecutionTurn {
            turn_id: "turn-stream-recovery".to_string(),
            turn_seq: 1,
            accepted_at: ts(1_500),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("请输出完整回复".to_string()),
            items: vec![session_turn_item(
                "user_message",
                "completed",
                None,
                Some("请输出完整回复".to_string()),
                Some("user-stream-recovery".to_string()),
                orchestrator_thread_id,
            )],
        },
    );
    let client = InterruptedThenRecoveredSessionModelBridgeClient {
        streaming_calls: AtomicUsize::new(0),
        non_stream_calls: AtomicUsize::new(0),
        recovery_messages: Mutex::new(Vec::new()),
    };
    let event_bus = InMemoryEventBus::new(32);
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: "turn-stream-recovery".to_string(),
        workspace_id: None,
        prompt: "请输出完整回复".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };

    let output = run_session_turn_execution_for_test(SessionTurnExecutionRuntime {
        client: &client,
        event_bus: &event_bus,
        session_store: &store,
        conversation_registry: conversation_registry.as_ref(),
        plan_store: &crate::test_plan_store("test-plan"),
        settings_store: None,
        safety_gate: None,
        skill_runtime: None,
        request,
        prompt: "请输出完整回复".to_string(),
        knowledge_context_prompt: None,
        persist_session_state: None,
        live_settings_store: None,
        extension_hooks: None,
    })
    .expect("流中断应由非流式降级完成");

    assert_eq!(output.final_content, "已通过非流式降级完成。");
    assert_eq!(
        client.streaming_calls.load(Ordering::SeqCst),
        MODEL_STREAM_INTERRUPTION_RECOVERY_MAX_ATTEMPTS + 1
    );
    assert_eq!(client.non_stream_calls.load(Ordering::SeqCst), 1);
    let recovery_messages = client
        .recovery_messages
        .lock()
        .expect("recovery messages mutex poisoned");
    assert_eq!(
        recovery_messages
            .iter()
            .filter(|message| {
                message.role == "user"
                    && message
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains("已保留此前可见内容"))
            })
            .count(),
        1,
        "重复恢复只保留当前请求控制提示"
    );
    let turn = store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .expect("completed turn should remain visible");
    assert_eq!(turn.status, "completed");
    assert!(turn.items.iter().all(|item| item.kind != "assistant_error"));
}

#[test]
fn image_session_turn_streaming_error_uses_image_capability_message() {
    let session_id = SessionId::new("session-image-error-layer");
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "image error layer")
        .expect("session should be creatable");
    let (_mission_id, orchestrator_thread_id) =
        store.ensure_session_mission(&session_id, ts(920), || {
            magi_core::MissionId::new("mission-image-error-layer")
        });
    let conversation_registry = Arc::new(ConversationRegistry::new());
    seed_conversation_turn(
        &store,
        &session_id,
        conversation_registry.turn_coordinator(),
        ActiveExecutionTurn {
            turn_id: "turn-image-error-layer".to_string(),
            turn_seq: 1,
            accepted_at: ts(1_000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("请识别这张图片".to_string()),
            items: vec![session_turn_item(
                "user_message",
                "completed",
                None,
                Some("请识别这张图片".to_string()),
                Some("user-image-error-layer".to_string()),
                orchestrator_thread_id,
            )],
        },
    );
    let client = FailingModelBridgeClient {
        message: "provider response invalid: empty stream response".to_string(),
    };
    let event_bus = InMemoryEventBus::new(16);
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: "turn-image-error-layer".to_string(),
        workspace_id: None,
        prompt: "请识别这张图片".to_string(),
        images: vec![
            SessionTurnImage::from_data_url("smoke.png", "data:image/png;base64,iVBORw0KGgo=")
                .expect("image should parse"),
        ],
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };

    let error = match run_session_turn_execution_for_test(SessionTurnExecutionRuntime {
        client: &client,
        event_bus: &event_bus,
        session_store: &store,
        conversation_registry: conversation_registry.as_ref(),
        plan_store: &crate::test_plan_store("test-plan"),
        settings_store: None,
        safety_gate: None,
        skill_runtime: None,
        request,
        prompt: "请识别这张图片".to_string(),
        knowledge_context_prompt: None,
        persist_session_state: None,
        live_settings_store: None,
        extension_hooks: None,
    }) {
        Ok(_) => panic!("image provider empty stream should fail"),
        Err(error) => error,
    };

    assert_eq!(
        error.public_message,
        crate::model_error::PUBLIC_MODEL_IMAGE_INVOCATION_FAILURE_MESSAGE
    );
    assert_eq!(
        error.reason,
        SessionTurnFailureReason::ModelImageInvocationFailed
    );
    let turn = store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .expect("failed turn should remain visible");
    assert_eq!(turn.status, "failed");
    assert!(turn.items.iter().any(|item| {
        item.kind == "assistant_error"
            && item.content.as_deref() == Some(error.public_message.as_str())
    }));
}

#[test]
fn stream_session_turn_round_reuses_accepted_assistant_placeholder() {
    // 验证流式首段 assistant text 用 request.placeholder_message_id 作为 item_id。
    // 历史方案曾在 accept 阶段把 placeholder 以 item_seq=2 预占进 turn.items，
    // 现在不再预占——首个 text delta 走 upsert，按 max(item_seq)+1=2 自然创建。
    let session_id = SessionId::new("session-placeholder-reuse");
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "placeholder reuse")
        .expect("session should be creatable");
    let (_, orchestrator_thread_id) = store.ensure_session_mission(&session_id, ts(900), || {
        magi_core::MissionId::new("mission-placeholder-reuse")
    });
    let mut user_item = session_turn_item(
        "user_message",
        "completed",
        None,
        Some("请只回复一句话".to_string()),
        Some("user-placeholder-reuse".to_string()),
        orchestrator_thread_id.clone(),
    );
    user_item.item_seq = 1;
    let conversation_registry = ConversationRegistry::new();
    seed_conversation_turn(
        &store,
        &session_id,
        conversation_registry.turn_coordinator(),
        ActiveExecutionTurn {
            turn_id: "turn-placeholder-reuse".to_string(),
            turn_seq: 1000,
            accepted_at: ts(1000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("请只回复一句话".to_string()),
            items: vec![user_item],
        },
    );
    let event_bus = InMemoryEventBus::new(16);
    let client = StreamingTextModelBridgeClient {
        delta_content: "你好".to_string(),
        payload: serde_json::json!({ "content": "你好" }).to_string(),
    };
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: "turn-placeholder-reuse".to_string(),
        workspace_id: None,
        prompt: "请只回复一句话".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: Some("request-placeholder-reuse".to_string()),
        user_message_id: Some("user-placeholder-reuse".to_string()),
        placeholder_message_id: Some("assistant-placeholder-reuse".to_string()),
        command: None,
    };
    let usage_binding = session_turn_model_usage_binding(false);
    let mut messages = vec![ChatMessage {
        input_request_id: None,
        context_origin: Default::default(),
        role: "user".to_string(),
        content: Some(request.prompt.clone()),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_context: Vec::new(),
    }];

    let output = stream_session_turn_round(SessionTurnRoundRuntime {
        history_generation: 0,
        client: &client,
        event_bus: &event_bus,
        session_store: &store,
        settings_store: None,
        request: &request,
        usage_binding: &usage_binding,
        prompt: &request.prompt,
        messages: &mut messages,
        request_controls: &mut crate::request_controls::RequestControls::default(),
        provider_retry_attempts: &std::cell::Cell::new(0),
        stream_interruption_recovery_attempts: 0,
        round: 0,
        context_window_tokens: 256_000,
        orchestrator_thread_id: &orchestrator_thread_id,
        persist_session_state: None,
        extension_hooks: None,
    })
    .expect("round should stream");

    assert_eq!(
        output.final_item_id.as_deref(),
        Some("assistant-placeholder-reuse"),
        "首段 assistant 文本必须以 request.placeholder_message_id 作为 item_id"
    );
    let canonical_turn = store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .find(|turn| turn.turn_id == "turn-placeholder-reuse")
        .expect("canonical turn should be stored");
    let assistant_items = canonical_turn
        .items
        .iter()
        .filter(|item| item.kind == CanonicalTurnItemKind::AssistantText)
        .collect::<Vec<_>>();
    assert_eq!(
        assistant_items.len(),
        1,
        "流式正文不能新增第二条 assistant item"
    );
    assert_eq!(assistant_items[0].item_id, "assistant-placeholder-reuse");
    // accept 阶段只写 user_message(seq=1)；流式正文是首个新 item，拿到 max+1=2。
    // 同 round 内的 thinking 即便后到（非增量 reasoning provider 在 post-streaming
    // 才补 item），由 projection 层按 kind 重排为 thinking → text，不依赖 item_seq。
    assert_eq!(
        assistant_items[0].item_seq, 2,
        "stream text 是首个 assistant item，应分到 item_seq=2（user=1, text=2）"
    );
    assert_eq!(
        assistant_items[0].status,
        CanonicalTurnItemStatus::Completed
    );
    assert_eq!(assistant_items[0].content.as_deref(), Some("你好"));
    assert_eq!(
        assistant_items[0].metadata.get("modelRound"),
        Some(&serde_json::Value::from(0_u64)),
        "流式正文必须携带模型轮次，供前端将下一轮 thinking 与本轮正文分隔"
    );
}

#[test]
fn session_turn_round_forwards_model_retry_runtime_events() {
    let session_id = SessionId::new("session-model-retry-runtime");
    let workspace_id = Some(WorkspaceId::new("workspace-model-retry-runtime"));
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "model retry runtime")
        .expect("session should be creatable");
    let (_, orchestrator_thread_id) = store.ensure_session_mission(&session_id, ts(900), || {
        magi_core::MissionId::new("mission-model-retry-runtime")
    });
    let conversation_registry = ConversationRegistry::new();
    seed_conversation_turn(
        &store,
        &session_id,
        conversation_registry.turn_coordinator(),
        ActiveExecutionTurn {
            turn_id: "turn-model-retry-runtime".to_string(),
            turn_seq: 1_000,
            accepted_at: ts(1_000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("请继续".to_string()),
            items: vec![session_turn_item(
                "user_message",
                "completed",
                None,
                Some("请继续".to_string()),
                Some("user-model-retry-runtime".to_string()),
                orchestrator_thread_id.clone(),
            )],
        },
    );
    let event_bus = InMemoryEventBus::new(16);
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: "turn-model-retry-runtime".to_string(),
        workspace_id: workspace_id.clone(),
        prompt: "请继续".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: Some("assistant-model-retry-runtime".to_string()),
        command: None,
    };
    let usage_binding = session_turn_model_usage_binding(false);
    let mut messages = vec![ChatMessage {
        input_request_id: None,
        context_origin: Default::default(),
        role: "user".to_string(),
        content: Some(request.prompt.clone()),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_context: Vec::new(),
    }];

    stream_session_turn_round(SessionTurnRoundRuntime {
        history_generation: 0,
        client: &RetryEventModelBridgeClient,
        event_bus: &event_bus,
        session_store: &store,
        settings_store: None,
        request: &request,
        usage_binding: &usage_binding,
        prompt: &request.prompt,
        messages: &mut messages,
        request_controls: &mut crate::request_controls::RequestControls::default(),
        provider_retry_attempts: &std::cell::Cell::new(0),
        stream_interruption_recovery_attempts: 0,
        round: 0,
        context_window_tokens: 256_000,
        orchestrator_thread_id: &orchestrator_thread_id,
        persist_session_state: None,
        extension_hooks: None,
    })
    .expect("round should complete after retry");

    let retry_events = event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .filter(|event| event.event_type == "model.retry.runtime")
        .collect::<Vec<_>>();
    assert_eq!(retry_events.len(), 3);
    assert_eq!(retry_events[0].payload["phase"], "scheduled");
    assert_eq!(retry_events[1].payload["phase"], "attempt_started");
    assert_eq!(retry_events[2].payload["phase"], "settled");
    assert!(retry_events.iter().all(|event| {
        event.payload["message_id"] == "assistant-model-retry-runtime"
            && event.session_id.as_ref() == Some(&session_id)
            && event.workspace_id.as_ref() == workspace_id.as_ref()
    }));
}

#[test]
fn session_turn_messages_include_persisted_history_before_current_turn() {
    let session_id = SessionId::new("session-context-history");
    let store = SessionStore::from_state_for_test(SessionStoreState {
        inputs: Vec::new(),
        current_session_id: Some(session_id.clone()),
        sessions: vec![SessionRecord {
            session_id: session_id.clone(),
            title: "context history".to_string(),
            status: SessionLifecycleStatus::Active,
            created_at: ts(900),
            updated_at: ts(2000),
            message_count: None,
            workspace_id: None,
            last_completed_at: None,
            last_viewed_at: None,
            kind: Default::default(),
        }],
        timeline: vec![
            TimelineEntry {
                entry_id: "timeline-user-prev".to_string(),
                session_id: session_id.clone(),
                kind: TimelineEntryKind::UserMessage,
                message: "请用一句话回答：2+3 等于几？".to_string(),
                occurred_at: ts(1000),
            },
            TimelineEntry {
                entry_id: "timeline-assistant-prev".to_string(),
                session_id: session_id.clone(),
                kind: TimelineEntryKind::AssistantMessage,
                message: "timeline snapshot 不应作为模型上下文事实源".to_string(),
                occurred_at: ts(1200),
            },
            TimelineEntry {
                entry_id: "timeline-user-current".to_string(),
                session_id: session_id.clone(),
                kind: TimelineEntryKind::UserMessage,
                message: "请基于上一轮结果，用一句话回答：再加 4 等于几？".to_string(),
                occurred_at: ts(2000),
            },
        ],
        canonical_turns: vec![CanonicalTurn {
            model_steps: Vec::new(),
            session_id: session_id.clone(),
            turn_id: "turn-prev".to_string(),
            turn_seq: 1000,
            accepted_at: ts(1000),
            completed_at: Some(ts(1200)),
            status: CanonicalTurnStatus::Completed,
            response_duration_ms: Some(200),
            usage: None,
            items: vec![
                CanonicalTurnItem {
                    session_id: session_id.clone(),
                    turn_id: "turn-prev".to_string(),
                    turn_seq: 1000,
                    item_id: "turn-item-prev-user".to_string(),
                    item_seq: 1,
                    kind: CanonicalTurnItemKind::UserMessage,
                    created_at: ts(1000),
                    status: CanonicalTurnItemStatus::Completed,
                    item_version: None,
                    updated_at: ts(1000),
                    title: None,
                    content: Some("请用一句话回答：2+3 等于几？".to_string()),
                    blocks: Vec::new(),
                    tool: None,
                    worker: None,
                    source_thread_id: magi_core::ThreadId::new("thread-test-orchestrator"),
                    visibility: CanonicalTurnVisibility::default(),
                    metadata: HashMap::from([(
                        "images".to_string(),
                        serde_json::json!([{
                            "name": "previous.png",
                            "dataUrl": "data:image/png;base64,AAA"
                        }]),
                    )]),
                },
                CanonicalTurnItem {
                    session_id: session_id.clone(),
                    turn_id: "turn-prev".to_string(),
                    turn_seq: 1000,
                    item_id: "turn-item-prev-final".to_string(),
                    item_seq: 2,
                    kind: CanonicalTurnItemKind::AssistantText,
                    created_at: ts(1200),
                    status: CanonicalTurnItemStatus::Completed,
                    item_version: None,
                    updated_at: ts(1200),
                    title: None,
                    content: Some("2+3 等于 5。".to_string()),
                    blocks: Vec::new(),
                    tool: None,
                    worker: Some(CanonicalWorkerRef {
                        task_id: Some(TaskId::new("task-root-context-history")),
                        worker_id: None,
                        role_id: None,
                        title: Some("最终回复".to_string()),
                    }),
                    source_thread_id: magi_core::ThreadId::new(
                        "thread-coordinator-task-root-context-history",
                    ),
                    visibility: CanonicalTurnVisibility::default(),
                    metadata: HashMap::from([(
                        "assistantOutputKind".to_string(),
                        serde_json::json!("final"),
                    )]),
                },
            ],
            metadata: HashMap::new(),
        }],
        notifications: Vec::new(),
        goals: Vec::new(),
        plans: Vec::new(),
        execution_sidecar_store: Default::default(),
        thread_context_checkpoints: vec![],
        thread_registry: vec![ExecutionThread {
            thread_id: magi_core::ThreadId::new("thread-test-orchestrator"),
            session_id: session_id.clone(),
            mission_id: magi_core::MissionId::new("mission-context-history"),
            role_id: ORCHESTRATOR_ROLE_ID.to_string(),
            worker_instance_id: magi_core::WorkerId::new("worker-orchestrator-test"),
            status: ExecutionThreadStatus::Idle,
            created_at: ts(900),
            last_used_at: ts(1200),
            observed_context_window_tokens: None,
            handled_task_ids: Vec::new(),
        }],
    });
    let coordinator = SessionTurnCoordinator::new();
    seed_conversation_turn(
        &store,
        &session_id,
        &coordinator,
        ActiveExecutionTurn {
            turn_id: "turn-session-2000".to_string(),
            turn_seq: 2000,
            accepted_at: ts(2000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("请基于上一轮结果，用一句话回答：再加 4 等于几？".to_string()),
            items: Vec::new(),
        },
    );

    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id,
        turn_id: "turn-session-2000".to_string(),
        workspace_id: None,
        prompt: "请基于上一轮结果，用一句话回答：再加 4 等于几？".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };
    let history = projected_test_history(&store, &request);
    let messages = build_session_turn_messages(&store, &request, &request.prompt, None, &history);

    assert_eq!(
        messages
            .iter()
            .map(|message| message.role.as_str())
            .collect::<Vec<_>>(),
        vec![
            "developer",
            "developer",
            "developer",
            "user",
            "assistant",
            "user",
        ]
    );
    let history_user_index = messages
        .iter()
        .position(|message| {
            message.role == "user"
                && message.content.as_deref() == Some("请用一句话回答：2+3 等于几？")
        })
        .expect("应保留历史用户消息");
    let history_assistant_index = messages
        .iter()
        .position(|message| {
            message.role == "assistant" && message.content.as_deref() == Some("2+3 等于 5。")
        })
        .expect("应保留历史助手消息");
    let current_user_index = messages
        .iter()
        .rposition(|message| {
            message.role == "user"
                && message.content.as_deref()
                    == Some("请基于上一轮结果，用一句话回答：再加 4 等于几？")
        })
        .expect("应保留当前用户消息");
    assert!(history_user_index < history_assistant_index);
    assert!(history_assistant_index < current_user_index);
    assert!(
        messages[..history_user_index]
            .iter()
            .all(|message| message.role == "developer")
    );
    assert!(messages[history_user_index].images.is_empty());
    assert!(messages.iter().any(|message| {
        message.role == "developer"
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("当前执行权限快照"))
    }));
    assert!(messages.iter().any(|message| {
        message.role == "developer"
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("本轮用户原始输入"))
    }));
}

#[test]
fn session_turn_normalizes_interrupted_tool_history_before_next_model_call() {
    let session_id = SessionId::new("session-interrupted-tool-history");
    let thread_id = magi_core::ThreadId::new("thread-interrupted-tool-history");
    let tool_call_id = "call-interrupted-shell".to_string();
    let store = SessionStore::from_state_for_test(SessionStoreState {
        inputs: Vec::new(),
        current_session_id: Some(session_id.clone()),
        sessions: vec![SessionRecord {
            session_id: session_id.clone(),
            title: "interrupted tool history".to_string(),
            status: SessionLifecycleStatus::Active,
            created_at: ts(900),
            updated_at: ts(1_100),
            message_count: None,
            workspace_id: None,
            last_completed_at: None,
            last_viewed_at: None,
            kind: Default::default(),
        }],
        timeline: Vec::new(),
        canonical_turns: vec![CanonicalTurn {
            model_steps: Vec::new(),
            session_id: session_id.clone(),
            turn_id: "turn-interrupted-tool".to_string(),
            turn_seq: 1_000,
            accepted_at: ts(1_000),
            completed_at: Some(ts(1_100)),
            status: CanonicalTurnStatus::Cancelled,
            response_duration_ms: Some(100),
            usage: None,
            items: vec![CanonicalTurnItem {
                session_id: session_id.clone(),
                turn_id: "turn-interrupted-tool".to_string(),
                turn_seq: 1_000,
                item_id: "turn-item-interrupted-tool".to_string(),
                item_seq: 2,
                kind: CanonicalTurnItemKind::ToolCall,
                created_at: ts(1_000),
                status: CanonicalTurnItemStatus::Cancelled,
                item_version: None,
                updated_at: ts(1_100),
                title: Some("shell".to_string()),
                content: Some("正在调用工具：shell".to_string()),
                blocks: Vec::new(),
                tool: Some(CanonicalToolCall {
                    call_id: tool_call_id.clone(),
                    name: "shell".to_string(),
                    arguments: Some(serde_json::json!({
                        "command": "sleep 20; printf done"
                    })),
                    result: None,
                    error: None,
                }),
                worker: None,
                source_thread_id: thread_id.clone(),
                visibility: CanonicalTurnVisibility::default(),
                metadata: HashMap::new(),
            }],
            metadata: HashMap::new(),
        }],
        notifications: Vec::new(),
        goals: Vec::new(),
        plans: Vec::new(),
        execution_sidecar_store: Default::default(),
        thread_context_checkpoints: vec![],
        thread_registry: vec![ExecutionThread {
            thread_id: thread_id.clone(),
            session_id: session_id.clone(),
            mission_id: magi_core::MissionId::new("mission-interrupted-tool-history"),
            role_id: ORCHESTRATOR_ROLE_ID.to_string(),
            worker_instance_id: magi_core::WorkerId::new("worker-interrupted-tool-history"),
            status: ExecutionThreadStatus::Idle,
            created_at: ts(900),
            last_used_at: ts(1_100),
            observed_context_window_tokens: None,
            handled_task_ids: Vec::new(),
        }],
    });

    let history = store
        .thread_context_snapshot(&thread_id)
        .unwrap()
        .messages()
        .to_vec();
    assert_eq!(
        history
            .iter()
            .map(|message| message.role.as_str())
            .collect::<Vec<_>>(),
        vec!["assistant", "tool"]
    );
    assert!(history[1].content.as_deref().is_some_and(|content| {
        content.contains(r#""status":"interrupted""#)
            && content.contains(r#""execution":"unknown""#)
    }));
    assert_eq!(
        store
            .thread_context_snapshot(&thread_id)
            .unwrap()
            .messages(),
        history,
        "重复读取不得再次插入中断结果"
    );

    // 没有 canonical 证据时不推测工具执行结果。
    let mut empty = store.export_state();
    empty.canonical_turns.clear();
    let empty = SessionStore::from_state_for_test(empty);
    assert!(
        empty
            .thread_context_snapshot(&thread_id)
            .unwrap()
            .messages()
            .is_empty()
    );
}

#[test]
fn interrupted_while_awaiting_approval_is_not_treated_as_possibly_executed() {
    let session_id = SessionId::new("session-awaiting-approval-history");
    let thread_id = magi_core::ThreadId::new("thread-awaiting-approval-history");
    let tool_call_id = "call-interrupted-shell".to_string();
    let tool_arguments = r#"{"command":"rm -rf build"}"#.to_string();
    let store = SessionStore::from_state_for_test(SessionStoreState {
        inputs: Vec::new(),
        current_session_id: Some(session_id.clone()),
        sessions: vec![SessionRecord {
            session_id: session_id.clone(),
            title: "interrupted tool history".to_string(),
            status: SessionLifecycleStatus::Active,
            created_at: ts(900),
            updated_at: ts(1_100),
            message_count: None,
            workspace_id: None,
            last_completed_at: None,
            last_viewed_at: None,
            kind: Default::default(),
        }],
        timeline: Vec::new(),
        canonical_turns: vec![CanonicalTurn {
            model_steps: Vec::new(),
            session_id: session_id.clone(),
            turn_id: "turn-interrupted-tool".to_string(),
            turn_seq: 1_000,
            accepted_at: ts(1_000),
            completed_at: Some(ts(1_100)),
            status: CanonicalTurnStatus::Cancelled,
            response_duration_ms: Some(100),
            usage: None,
            items: vec![CanonicalTurnItem {
                session_id: session_id.clone(),
                turn_id: "turn-interrupted-tool".to_string(),
                turn_seq: 1_000,
                item_id: "turn-item-interrupted-tool".to_string(),
                item_seq: 2,
                kind: CanonicalTurnItemKind::ToolCall,
                created_at: ts(1_000),
                status: CanonicalTurnItemStatus::Cancelled,
                item_version: None,
                updated_at: ts(1_100),
                title: Some("shell".to_string()),
                content: Some("正在调用工具：shell".to_string()),
                blocks: Vec::new(),
                tool: Some(CanonicalToolCall {
                    call_id: tool_call_id.clone(),
                    name: "shell".to_string(),
                    arguments: Some(serde_json::json!({
                        "command": "rm -rf build"
                    })),
                    result: Some(serde_json::json!({
                        "tool": "shell",
                        "status": "awaiting_approval",
                        "approval_id": "approval-interrupted",
                    })),
                    error: None,
                }),
                worker: None,
                source_thread_id: thread_id.clone(),
                visibility: CanonicalTurnVisibility::default(),
                metadata: HashMap::new(),
            }],
            metadata: HashMap::new(),
        }],
        notifications: Vec::new(),
        goals: Vec::new(),
        plans: Vec::new(),
        execution_sidecar_store: Default::default(),
        thread_context_checkpoints: vec![],
        thread_registry: vec![ExecutionThread {
            thread_id: thread_id.clone(),
            session_id: session_id.clone(),
            mission_id: magi_core::MissionId::new("mission-interrupted-tool-history"),
            role_id: ORCHESTRATOR_ROLE_ID.to_string(),
            worker_instance_id: magi_core::WorkerId::new("worker-interrupted-tool-history"),
            status: ExecutionThreadStatus::Idle,
            created_at: ts(900),
            last_used_at: ts(1_100),
            observed_context_window_tokens: None,
            handled_task_ids: Vec::new(),
        }],
    });

    let history = store
        .thread_context_snapshot(&thread_id)
        .unwrap()
        .messages()
        .to_vec();
    assert_eq!(
        history
            .iter()
            .map(|message| message.role.as_str())
            .collect::<Vec<_>>(),
        vec!["assistant", "tool"]
    );
    assert!(
        history[1].content.as_deref().is_some_and(|content| {
            content.contains(r#""status":"interrupted""#)
                && content.contains(r#""execution":"not_started""#)
        }),
        "等待授权时中断的调用尚未执行，不能标记为结果未知"
    );
    let ledger = crate::tool_execution_ledger::ToolExecutionLedger::from_thread_history(
        "继续", &history, None,
    );
    let retried = magi_bridge_client::ChatToolCall {
        id: "call-retry".to_string(),
        kind: "function".to_string(),
        function: magi_bridge_client::ChatToolFunction {
            name: "shell".to_string(),
            arguments: tool_arguments,
        },
    };
    assert!(
        matches!(
            ledger.plan(&[retried], None)[0],
            crate::tool_execution_ledger::ToolCallExecutionDecision::Execute { .. }
        ),
        "恢复后同一调用应能重新申请授权并执行，而不是被永久阻止"
    );
}

#[test]
fn session_turn_messages_keep_user_stopped_turn_with_interruption_marker() {
    let session_id = SessionId::new("session-context-history-cancelled");
    let thread_id = magi_core::ThreadId::new("thread-context-history-cancelled");
    let store = SessionStore::from_state_for_test(SessionStoreState {
        inputs: Vec::new(),
        current_session_id: Some(session_id.clone()),
        sessions: vec![SessionRecord {
            session_id: session_id.clone(),
            title: "cancelled context history".to_string(),
            status: SessionLifecycleStatus::Active,
            created_at: ts(900),
            updated_at: ts(2_000),
            message_count: None,
            workspace_id: None,
            last_completed_at: None,
            last_viewed_at: None,
            kind: Default::default(),
        }],
        timeline: Vec::new(),
        canonical_turns: vec![CanonicalTurn {
            model_steps: Vec::new(),
            session_id: session_id.clone(),
            turn_id: "turn-cancelled".to_string(),
            turn_seq: 1_000,
            accepted_at: ts(1_000),
            completed_at: Some(ts(1_100)),
            status: CanonicalTurnStatus::Cancelled,
            response_duration_ms: Some(100),
            usage: None,
            items: vec![CanonicalTurnItem {
                session_id: session_id.clone(),
                turn_id: "turn-cancelled".to_string(),
                turn_seq: 1_000,
                item_id: "turn-cancelled-user".to_string(),
                item_seq: 1,
                kind: CanonicalTurnItemKind::UserMessage,
                created_at: ts(1_000),
                status: CanonicalTurnItemStatus::Cancelled,
                item_version: None,
                updated_at: ts(1_100),
                title: None,
                content: Some("执行 sleep 20，完成后回复未被停止".to_string()),
                blocks: Vec::new(),
                tool: None,
                worker: None,
                source_thread_id: thread_id.clone(),
                visibility: CanonicalTurnVisibility::default(),
                metadata: HashMap::new(),
            }],
            metadata: HashMap::new(),
        }],
        notifications: Vec::new(),
        goals: Vec::new(),
        plans: Vec::new(),
        execution_sidecar_store: Default::default(),
        thread_context_checkpoints: vec![],
        thread_registry: vec![ExecutionThread {
            thread_id,
            session_id: session_id.clone(),
            mission_id: magi_core::MissionId::new("mission-context-history-cancelled"),
            role_id: ORCHESTRATOR_ROLE_ID.to_string(),
            worker_instance_id: magi_core::WorkerId::new("worker-context-history-cancelled"),
            status: ExecutionThreadStatus::Idle,
            created_at: ts(900),
            last_used_at: ts(1_100),
            observed_context_window_tokens: None,
            handled_task_ids: Vec::new(),
        }],
    });
    let coordinator = SessionTurnCoordinator::new();
    seed_conversation_turn(
        &store,
        &session_id,
        &coordinator,
        ActiveExecutionTurn {
            turn_id: "turn-current".to_string(),
            turn_seq: 2_000,
            accepted_at: ts(2_000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("只回复停止后恢复正常".to_string()),
            items: Vec::new(),
        },
    );
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id,
        turn_id: "turn-current".to_string(),
        workspace_id: None,
        prompt: "只回复停止后恢复正常".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };

    let history = projected_test_history(&store, &request);
    let messages = build_session_turn_messages(&store, &request, &request.prompt, None, &history);
    let contents = messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>();

    let stopped_request = contents
        .iter()
        .position(|content| content.contains("sleep 20"))
        .expect("用户停止的轮次必须保留在后续上下文中");
    assert!(
        contents[stopped_request..]
            .iter()
            .any(|content| content.contains("已被用户中断")),
        "停止的轮次必须明确标注为被用户中断"
    );
    assert_eq!(contents.last().copied(), Some("只回复停止后恢复正常"));
}

#[test]
fn session_turn_messages_do_not_drop_early_history_after_six_rounds() {
    let session_id = SessionId::new("session-context-history-long");
    let thread_id = magi_core::ThreadId::new("thread-context-history-long");
    let canonical_turns = (0..7)
        .map(|index| {
            let turn_seq = 1_000 + index * 100;
            CanonicalTurn {
                model_steps: Vec::new(),
                session_id: session_id.clone(),
                turn_id: format!("turn-history-{index}"),
                turn_seq,
                accepted_at: ts(turn_seq),
                completed_at: Some(ts(turn_seq + 10)),
                status: CanonicalTurnStatus::Completed,
                response_duration_ms: Some(10),
                usage: None,
                items: vec![
                    CanonicalTurnItem {
                        session_id: session_id.clone(),
                        turn_id: format!("turn-history-{index}"),
                        turn_seq,
                        item_id: format!("history-user-{index}"),
                        item_seq: 1,
                        kind: CanonicalTurnItemKind::UserMessage,
                        created_at: ts(turn_seq),
                        status: CanonicalTurnItemStatus::Completed,
                        item_version: None,
                        updated_at: ts(turn_seq),
                        title: None,
                        content: Some(if index == 0 {
                            "最早上下文标记：银杏-7429-海盐".to_string()
                        } else {
                            format!("第 {index} 轮用户消息")
                        }),
                        blocks: Vec::new(),
                        tool: None,
                        worker: None,
                        source_thread_id: thread_id.clone(),
                        visibility: CanonicalTurnVisibility::default(),
                        metadata: HashMap::new(),
                    },
                    CanonicalTurnItem {
                        session_id: session_id.clone(),
                        turn_id: format!("turn-history-{index}"),
                        turn_seq,
                        item_id: format!("history-assistant-{index}"),
                        item_seq: 2,
                        kind: CanonicalTurnItemKind::AssistantText,
                        created_at: ts(turn_seq + 10),
                        status: CanonicalTurnItemStatus::Completed,
                        item_version: None,
                        updated_at: ts(turn_seq + 10),
                        title: None,
                        content: Some(format!("第 {index} 轮助手回复")),
                        blocks: Vec::new(),
                        tool: None,
                        worker: None,
                        source_thread_id: thread_id.clone(),
                        visibility: CanonicalTurnVisibility::default(),
                        metadata: HashMap::new(),
                    },
                ],
                metadata: HashMap::new(),
            }
        })
        .collect::<Vec<_>>();
    let store = SessionStore::from_state_for_test(SessionStoreState {
        inputs: Vec::new(),
        current_session_id: Some(session_id.clone()),
        sessions: vec![SessionRecord {
            session_id: session_id.clone(),
            title: "long context history".to_string(),
            status: SessionLifecycleStatus::Active,
            created_at: ts(900),
            updated_at: ts(2_000),
            message_count: None,
            workspace_id: None,
            last_completed_at: None,
            last_viewed_at: None,
            kind: Default::default(),
        }],
        timeline: Vec::new(),
        canonical_turns,
        notifications: Vec::new(),
        goals: Vec::new(),
        plans: Vec::new(),
        execution_sidecar_store: Default::default(),
        thread_context_checkpoints: vec![],
        thread_registry: vec![ExecutionThread {
            thread_id: thread_id.clone(),
            session_id: session_id.clone(),
            mission_id: magi_core::MissionId::new("mission-context-history-long"),
            role_id: ORCHESTRATOR_ROLE_ID.to_string(),
            worker_instance_id: magi_core::WorkerId::new("worker-context-history-long"),
            status: ExecutionThreadStatus::Idle,
            created_at: ts(900),
            last_used_at: ts(1_700),
            observed_context_window_tokens: None,
            handled_task_ids: Vec::new(),
        }],
    });
    let coordinator = SessionTurnCoordinator::new();
    seed_conversation_turn(
        &store,
        &session_id,
        &coordinator,
        ActiveExecutionTurn {
            turn_id: "turn-session-2000".to_string(),
            turn_seq: 2_000,
            accepted_at: ts(2_000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("最早的上下文标记是什么？".to_string()),
            items: Vec::new(),
        },
    );
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id,
        turn_id: "turn-session-2000".to_string(),
        workspace_id: None,
        prompt: "最早的上下文标记是什么？".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };

    let history = projected_test_history(&store, &request);
    let messages = build_session_turn_messages(&store, &request, &request.prompt, None, &history);

    assert!(messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("银杏-7429-海盐"))
    }));
}

#[test]
fn build_session_turn_messages_injects_structured_context_references() {
    let session_id = SessionId::new("session-context-references");
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "context references")
        .expect("session should be created");
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id,
        turn_id: "turn-context-references".to_string(),
        workspace_id: Some(WorkspaceId::new("workspace-context-references")),
        prompt: "分析引用内容".to_string(),
        images: Vec::new(),
        context_references: vec![crate::context_reference::SessionContextReference {
            kind: crate::context_reference::SessionContextReferenceKind::Directory,
            path: std::path::PathBuf::from("/tmp/external-reference"),
            source_folder_id: None,
            name: "external-reference".to_string(),
        }],
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };

    let history = projected_test_history(&store, &request);
    let messages = build_session_turn_messages(&store, &request, &request.prompt, None, &history);
    let reference_context = messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .find(|content| content.contains("/tmp/external-reference"))
        .expect("context reference prompt should be injected");
    assert!(reference_context.contains("只读上下文引用"));
    assert!(reference_context.contains("directory"));
    assert_eq!(
        messages
            .last()
            .and_then(|message| message.content.as_deref()),
        Some("分析引用内容")
    );
}

#[test]
fn build_session_turn_messages_injects_current_turn_knowledge_as_reference_fragment() {
    let session_id = SessionId::new("session-knowledge-context");
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "knowledge context")
        .expect("session should be created");
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id,
        turn_id: "turn-knowledge-context".to_string(),
        workspace_id: Some(WorkspaceId::new("workspace-knowledge-context")),
        prompt: "为什么采用单一事实源架构？".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };

    let messages = build_session_turn_messages(
        &store,
        &request,
        &request.prompt,
        Some("[reference:knowledge:adr] 单一事实源\n只读投影来自事件事实。"),
        &projected_test_history(&store, &request),
    );

    let knowledge = messages
        .iter()
        .find(|message| {
            message.role == "user"
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("id=\"knowledge_context\""))
        })
        .expect("knowledge context should be injected as a user reference fragment");
    assert!(
        knowledge
            .content
            .as_deref()
            .is_some_and(|content| content.contains("只读投影来自事件事实"))
    );
    assert_eq!(
        messages
            .last()
            .and_then(|message| message.content.as_deref()),
        Some("为什么采用单一事实源架构？")
    );
}

#[test]
fn build_session_turn_messages_attaches_current_user_images() {
    let session_id = SessionId::new("session-current-image");
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "current image")
        .expect("session should be created");
    let coordinator = SessionTurnCoordinator::new();
    seed_conversation_turn(
        &store,
        &session_id,
        &coordinator,
        ActiveExecutionTurn {
            turn_id: "turn-current-image".to_string(),
            turn_seq: 1,
            accepted_at: ts(1000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("识别图片".to_string()),
            items: Vec::new(),
        },
    );
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id,
        turn_id: "turn-current-image".to_string(),
        workspace_id: None,
        prompt: "识别图片".to_string(),
        images: vec![
            SessionTurnImage::from_data_url("paste.png", "data:image/png;base64,AAA")
                .expect("image should parse"),
        ],
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };

    let history = projected_test_history(&store, &request);
    let messages = build_session_turn_messages(&store, &request, &request.prompt, None, &history);
    let current_user_message = messages.last().expect("current user message");

    assert_eq!(current_user_message.role, "user");
    assert_eq!(current_user_message.content.as_deref(), Some("识别图片"));
    assert_eq!(current_user_message.images.len(), 1);
    assert_eq!(current_user_message.images[0].media_type, "image/png");
    assert_eq!(current_user_message.images[0].data, "AAA");
}

#[test]
fn build_session_turn_messages_only_contains_conversation_fragments() {
    let session_id = SessionId::new("session-workspace-chat");
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "workspace chat")
        .expect("session should be created");
    let coordinator = SessionTurnCoordinator::new();
    seed_conversation_turn(
        &store,
        &session_id,
        &coordinator,
        ActiveExecutionTurn {
            turn_id: "turn-workspace-chat".to_string(),
            turn_seq: 1,
            accepted_at: ts(1000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("解释一下当前状态".to_string()),
            items: Vec::new(),
        },
    );
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id,
        turn_id: "turn-workspace-chat".to_string(),
        workspace_id: Some(WorkspaceId::new("workspace-context")),
        prompt: "解释一下当前状态".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };

    let history = projected_test_history(&store, &request);
    let messages = build_session_turn_messages(&store, &request, &request.prompt, None, &history);

    assert_eq!(
        messages
            .iter()
            .map(|message| message.role.as_str())
            .collect::<Vec<_>>(),
        vec!["developer", "developer", "developer", "user"]
    );
    assert!(messages.iter().any(|message| {
        message.role == "developer"
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("上下文优先级"))
    }));
    assert_eq!(messages[3].content.as_deref(), Some("解释一下当前状态"));
}

#[test]
fn append_final_item_keeps_post_tool_assistant_item_separate_from_main_timeline_entry() {
    let session_id = SessionId::new("session-post-tool-final-item");
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "post tool final")
        .expect("session should be creatable");
    let (_mission_id, orchestrator_thread_id) =
        store.ensure_session_mission(&session_id, ts(1000), || {
            magi_core::MissionId::new("mission-post-tool-final-item")
        });
    let coordinator = SessionTurnCoordinator::new();
    seed_conversation_turn(
        &store,
        &session_id,
        &coordinator,
        ActiveExecutionTurn {
            turn_id: "turn-post-tool-final-item".to_string(),
            turn_seq: 1,
            accepted_at: ts(1000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("请调用工具后回答".to_string()),
            items: Vec::new(),
        },
    );
    let event_bus = InMemoryEventBus::new(16);
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: "turn-post-tool-final-item".to_string(),
        workspace_id: None,
        prompt: "请调用工具后回答".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: Some("request-post-tool-final-item".to_string()),
        user_message_id: Some("user-post-tool-final-item".to_string()),
        placeholder_message_id: Some("placeholder-post-tool-final-item".to_string()),
        command: None,
    };

    let mut pre_tool_stream = session_turn_item(
        "assistant_stream",
        "completed",
        Some("生成回复".to_string()),
        Some("我先检查工具结果。".to_string()),
        Some("turn-item-assistant-stream-main".to_string()),
        orchestrator_thread_id.clone(),
    );
    pre_tool_stream.timeline_entry_id = Some("turn-item-assistant-stream-main".to_string());
    upsert_session_turn_item_for_turn(
        &store,
        &session_id,
        Some(&request.turn_id),
        pre_tool_stream,
        None,
    )
    .expect("pre-tool stream item should be stored");
    let post_tool_stream = session_turn_item(
        "assistant_stream",
        "completed",
        Some("生成回复".to_string()),
        Some("工具结果显示可以继续。".to_string()),
        Some("turn-item-assistant-stream-post-tool".to_string()),
        orchestrator_thread_id.clone(),
    );
    upsert_session_turn_item_for_turn(
        &store,
        &session_id,
        Some(&request.turn_id),
        post_tool_stream,
        None,
    )
    .expect("post-tool stream item should be stored");

    append_final_item(
        TurnTerminalCommitPolicy::Executor,
        &event_bus,
        &store,
        &request,
        FinalItemInput {
            content: "最终答案来自工具后轮次。",
            item_id: Some("turn-item-assistant-stream-post-tool"),
            timeline_entry_id: Some("turn-item-assistant-stream-main"),
            model_round: Some(1),
        },
        &orchestrator_thread_id,
        None,
    )
    .expect("final item should be stored and published");

    let turn = store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .expect("current turn should remain available");
    let pre_tool_item = turn
        .items
        .iter()
        .find(|item| item.item_id == "turn-item-assistant-stream-main")
        .expect("pre-tool item should remain stored");
    assert_eq!(pre_tool_item.kind, "assistant_stream");
    assert_eq!(pre_tool_item.content.as_deref(), Some("我先检查工具结果。"));
    let post_tool_item = turn
        .items
        .iter()
        .find(|item| item.item_id == "turn-item-assistant-stream-post-tool")
        .expect("post-tool item should become final item");
    assert_eq!(post_tool_item.kind, "assistant_final");
    assert_eq!(
        post_tool_item.timeline_entry_id.as_deref(),
        Some("turn-item-assistant-stream-main")
    );
    assert_eq!(
        post_tool_item.content.as_deref(),
        Some("最终答案来自工具后轮次。")
    );
    assert_eq!(
        post_tool_item.metadata.get("modelRound"),
        Some(&serde_json::Value::from(1_u64)),
        "最终正文必须保留所属模型轮次，供前端按轮次分隔 thinking"
    );
    let canonical_turn = store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .find(|turn| turn.turn_id == "turn-post-tool-final-item")
        .expect("canonical turn should be stored");
    let canonical_post_tool_item = canonical_turn
        .items
        .iter()
        .find(|item| item.item_id == "turn-item-assistant-stream-post-tool")
        .expect("post-tool canonical assistant item should remain stored");
    assert_eq!(
        canonical_post_tool_item.metadata.get("modelRound"),
        Some(&serde_json::Value::from(1_u64)),
        "模型轮次必须进入 canonical 事件，不能只停留在运行时 sidecar"
    );
    assert_eq!(
        canonical_post_tool_item.kind,
        CanonicalTurnItemKind::AssistantText
    );
    assert_eq!(
        canonical_post_tool_item.status,
        CanonicalTurnItemStatus::Completed
    );
    assert_eq!(
        canonical_post_tool_item.content.as_deref(),
        Some("最终答案来自工具后轮次。")
    );
    assert!(
        store
            .timeline_for_session(&session_id)
            .iter()
            .all(|entry| !entry.message.contains("最终答案来自工具后轮次。")),
        "完成态不能再反向写 completed snapshot 作为正文事实源"
    );
}

#[test]
fn context_compaction_notice_is_persisted_as_renderable_system_item() {
    let session_id = SessionId::new("session-context-compaction-notice");
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "context compaction notice")
        .expect("session should be creatable");
    let (_, thread_id) = store.ensure_session_mission(&session_id, ts(1_000), || {
        magi_core::MissionId::new("mission-context-compaction-notice")
    });
    let coordinator = SessionTurnCoordinator::new();
    seed_conversation_turn(
        &store,
        &session_id,
        &coordinator,
        ActiveExecutionTurn {
            turn_id: "turn-context-compaction-notice".to_string(),
            turn_seq: 1,
            accepted_at: ts(1_000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("继续对话".to_string()),
            items: vec![session_turn_item(
                "user_message",
                "completed",
                None,
                Some("继续对话".to_string()),
                Some("user-context-compaction-notice".to_string()),
                thread_id.clone(),
            )],
        },
    );
    let event_bus = InMemoryEventBus::new(16);
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: "turn-context-compaction-notice".to_string(),
        workspace_id: Some(WorkspaceId::new("workspace-context-compaction-notice")),
        prompt: "继续对话".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };
    let item_id = new_context_compaction_item_id(&request.turn_id, &thread_id, "pre_turn");
    let writeback = ContextCompactionWritebackContext {
        event_bus: &event_bus,
        session_store: &store,
        session_id: &request.session_id,
        workspace_id: &request.workspace_id,
        thread_id: &thread_id,
        item_id: &item_id,
        phase: "pre_turn",
        persist_session_state: None,
        task: None,
        turn_visibility: None,
        expected_turn_id: None,
    };
    upsert_context_compaction_progress_notice(
        writeback,
        ContextCompactionProgress::Advanced {
            stage: "history_chunk",
            completed_chunks: 2,
            total_chunks: 4,
        },
    )
    .expect("上下文压缩进度应可写回");
    upsert_context_compaction_completed_notice(
        writeback,
        &ContextCompactionRecord {
            reason: "context_window_pressure",
            original_message_count: 42,
            compacted_message_count: 9,
            original_token_estimate: 180_000,
            compacted_token_estimate: 36_000,
            compacted_at: ts(1_001),
        },
    )
    .expect("上下文压缩完成事实应可写回");

    let turn = store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .find(|turn| turn.turn_id == request.turn_id)
        .expect("canonical turn should exist");
    let notice = turn
        .items
        .iter()
        .find(|item| item.kind == CanonicalTurnItemKind::SystemNotice)
        .expect("context compaction notice should exist");
    assert!(notice.visibility.renderable);
    assert_eq!(
        notice.metadata.get("noticeKind"),
        Some(&serde_json::json!("context_compaction"))
    );
    assert_eq!(
        notice.metadata.get("compactedTokenEstimate"),
        Some(&serde_json::json!(36_000))
    );
    assert_eq!(
        notice.metadata.get("compactionState"),
        Some(&serde_json::json!("completed"))
    );
    assert_eq!(
        turn.items
            .iter()
            .filter(|item| {
                item.metadata.get("noticeKind") == Some(&serde_json::json!("context_compaction"))
            })
            .count(),
        1,
        "压缩进度和终态必须更新同一个 canonical item"
    );
    assert!(
        event_bus
            .snapshot()
            .recent_events
            .iter()
            .any(|event| event.event_type == "session.turn.item")
    );
}

#[test]
fn context_limit_recovery_rebuilds_after_tool_fact_with_dynamic_target() {
    let session_id = SessionId::new("session-context-limit-after-tool");
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "context limit after tool")
        .expect("session should create");
    let (_, thread_id) = store.ensure_session_mission(&session_id, ts(1), || {
        magi_core::MissionId::new("mission-context-limit-after-tool")
    });
    let directory = tempfile::tempdir().expect("tempdir");
    let file_path = directory.path().join("facts.txt");
    std::fs::write(&file_path, "stable fact").expect("write file fact");
    let content_hash = magi_snapshot::path_content_hash(&file_path).expect("hash file fact");
    let mut history = vec![
        ThreadChatMessage {
            input_request_id: None,
            role: "user".to_string(),
            content: Some("读取 facts.txt 并继续完成任务".to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        },
        ThreadChatMessage {
            input_request_id: None,
            role: "assistant".to_string(),
            content: None,
            images: Vec::new(),
            tool_calls: vec![ThreadChatToolCall {
                id: "call-context-limit-file-read".to_string(),
                kind: "function".to_string(),
                function: ThreadChatToolFunction {
                    name: "file_read".to_string(),
                    arguments: serde_json::json!({"path": file_path}).to_string(),
                },
            }],
            tool_call_id: None,
            provider_context: Vec::new(),
        },
        ThreadChatMessage {
            input_request_id: None,
            role: "tool".to_string(),
            content: Some(
                serde_json::json!({
                    "tool": "file_read",
                    "status": "succeeded",
                    "path": file_path,
                    "content_hash": content_hash,
                    "content": "stable fact"
                })
                .to_string(),
            ),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some("call-context-limit-file-read".to_string()),
            provider_context: Vec::new(),
        },
    ];
    history.extend((0..24).map(|index| ThreadChatMessage {
        input_request_id: None,
        role: if index % 2 == 0 { "user" } else { "assistant" }.to_string(),
        content: Some("x".repeat(1_000)),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_context: Vec::new(),
    }));
    let store = crate::test_history::with_messages(&store, &thread_id, history, ts(2));
    let coordinator = SessionTurnCoordinator::new();
    seed_conversation_turn(
        &store,
        &session_id,
        &coordinator,
        ActiveExecutionTurn {
            turn_id: "turn-context-limit-after-tool".to_string(),
            turn_seq: 3,
            accepted_at: ts(3),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("继续".to_string()),
            items: vec![],
        },
    );
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: "turn-context-limit-after-tool".to_string(),
        workspace_id: Some(WorkspaceId::new("workspace-context-limit-after-tool")),
        prompt: "继续".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };
    let client = SemanticContextCompactionModelBridgeClient {
        requests: Mutex::new(Vec::new()),
    };
    let mut messages = Vec::new();
    let rebuilt = rebuild_messages_for_context_window(RebuildMessagesForContextWindowInput {
        mode: ContextCompactionMode::Recovery,
        identity_prompt: None,
        safety_gate: None,
        history_generation: &mut 0,
        client: &client,
        event_bus: &InMemoryEventBus::new(16),
        session_store: &store,
        request: &request,
        thread_id: &thread_id,
        prompt: &request.prompt,
        knowledge_context_prompt: None,
        context_window: 4_000,
        messages: &mut messages,
        request_context: &[],
        persist_session_state: None,
        settings_store: None,
        skill_runtime: None,
        persist_checkpoint: true,
        stable_contributions: &[],
        extension_hooks: None,
    });

    assert!(
        rebuilt.expect("上下文压缩应成功"),
        "小窗口恢复必须生成更小的上下文检查点"
    );
    let compaction_requests = client
        .requests
        .lock()
        .expect("context compaction requests mutex poisoned");
    assert_eq!(
        compaction_requests.len(),
        1,
        "同一次恢复只能执行一次有界语义压缩"
    );
    assert!(
        compaction_requests
            .iter()
            .any(|request| request.prompt.contains("file_read"))
    );
    assert!(messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("file_read 已成功读取"))
    }));
    assert!(store.thread_context_checkpoint(&thread_id).is_some());
}

#[test]
fn context_limit_recovery_returns_terminal_failure_instead_of_reusing_oversized_history() {
    let session_id = SessionId::new("session-context-compaction-failure");
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "context compaction failure")
        .expect("session should create");
    let (_, thread_id) = store.ensure_session_mission(&session_id, ts(1), || {
        magi_core::MissionId::new("mission-context-compaction-failure")
    });
    let store = crate::test_history::with_messages(
        &store,
        &thread_id,
        (0..32)
            .map(|index| ThreadChatMessage {
                input_request_id: None,
                role: if index % 2 == 0 { "user" } else { "assistant" }.to_string(),
                content: Some("x".repeat(1_000)),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            })
            .collect(),
        ts(2),
    );
    let coordinator = SessionTurnCoordinator::new();
    seed_conversation_turn(
        &store,
        &session_id,
        &coordinator,
        ActiveExecutionTurn {
            turn_id: "turn-context-compaction-failure".to_string(),
            turn_seq: 3,
            accepted_at: ts(3),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("继续".to_string()),
            items: vec![],
        },
    );
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: "turn-context-compaction-failure".to_string(),
        workspace_id: Some(WorkspaceId::new("workspace-context-compaction-failure")),
        prompt: "继续".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };
    let client = FailingContextCompactionModelBridgeClient {
        calls: AtomicUsize::new(0),
    };
    let event_bus = InMemoryEventBus::new(16);
    let mut messages = Vec::new();

    let result = rebuild_messages_for_context_window(RebuildMessagesForContextWindowInput {
        mode: ContextCompactionMode::Recovery,
        identity_prompt: None,
        safety_gate: None,
        history_generation: &mut 0,
        client: &client,
        event_bus: &event_bus,
        session_store: &store,
        request: &request,
        thread_id: &thread_id,
        prompt: &request.prompt,
        knowledge_context_prompt: None,
        context_window: 4_000,
        messages: &mut messages,
        request_context: &[],
        persist_session_state: None,
        settings_store: None,
        skill_runtime: None,
        persist_checkpoint: true,
        stable_contributions: &[],
        extension_hooks: None,
    });

    assert_eq!(result, Err(ContextCompactionTerminal::Failed));
    assert!(client.calls.load(Ordering::SeqCst) > 0);
    assert!(messages.is_empty(), "失败后不能重新使用超大原始上下文");
    assert!(store.thread_context_checkpoint(&thread_id).is_none());
    let turn = store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .find(|turn| turn.turn_id == request.turn_id)
        .expect("canonical turn should exist");
    let compaction_notice = turn
        .items
        .iter()
        .find(|item| {
            item.metadata.get("noticeKind") == Some(&serde_json::json!("context_compaction"))
        })
        .expect("failed compaction notice should exist");
    assert_eq!(compaction_notice.status, CanonicalTurnItemStatus::Failed);
    assert_eq!(
        compaction_notice.metadata.get("compactionState"),
        Some(&serde_json::json!("failed"))
    );
}

#[test]
fn context_limit_recovery_cancellation_does_not_install_checkpoint() {
    let session_id = SessionId::new("session-context-compaction-cancelled");
    let store = Arc::new(SessionStore::new());
    store
        .create_session(session_id.clone(), "context compaction cancelled")
        .expect("session should create");
    let (_, thread_id) = store.ensure_session_mission(&session_id, ts(1), || {
        magi_core::MissionId::new("mission-context-compaction-cancelled")
    });
    let store = Arc::new(crate::test_history::with_messages(
        &store,
        &thread_id,
        (0..32)
            .map(|index| ThreadChatMessage {
                input_request_id: None,
                role: if index % 2 == 0 { "user" } else { "assistant" }.to_string(),
                content: Some("x".repeat(1_000)),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            })
            .collect(),
        ts(2),
    ));
    let coordinator = SessionTurnCoordinator::new();
    seed_conversation_turn(
        store.as_ref(),
        &session_id,
        &coordinator,
        ActiveExecutionTurn {
            turn_id: "turn-context-compaction-cancelled".to_string(),
            turn_seq: 3,
            accepted_at: ts(3),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("继续".to_string()),
            items: vec![],
        },
    );
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: "turn-context-compaction-cancelled".to_string(),
        workspace_id: Some(WorkspaceId::new("workspace-context-compaction-cancelled")),
        prompt: "继续".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    };
    let client = CancellingContextCompactionModelBridgeClient {
        store: Arc::clone(&store),
        session_id: session_id.clone(),
    };
    let event_bus = InMemoryEventBus::new(16);
    let mut messages = Vec::new();

    let result = rebuild_messages_for_context_window(RebuildMessagesForContextWindowInput {
        mode: ContextCompactionMode::Recovery,
        identity_prompt: None,
        safety_gate: None,
        history_generation: &mut 0,
        client: &client,
        event_bus: &event_bus,
        session_store: &store,
        request: &request,
        thread_id: &thread_id,
        prompt: &request.prompt,
        knowledge_context_prompt: None,
        context_window: 4_000,
        messages: &mut messages,
        request_context: &[],
        persist_session_state: None,
        settings_store: None,
        skill_runtime: None,
        persist_checkpoint: true,
        stable_contributions: &[],
        extension_hooks: None,
    });

    assert_eq!(result, Err(ContextCompactionTerminal::Cancelled));
    assert!(messages.is_empty());
    assert!(store.thread_context_checkpoint(&thread_id).is_none());
    let turn = store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .find(|turn| turn.turn_id == request.turn_id)
        .expect("canonical turn should exist");
    let compaction_notice = turn
        .items
        .iter()
        .find(|item| {
            item.metadata.get("noticeKind") == Some(&serde_json::json!("context_compaction"))
        })
        .expect("cancelled compaction notice should exist");
    assert_eq!(compaction_notice.status, CanonicalTurnItemStatus::Cancelled);
    assert_eq!(
        compaction_notice.metadata.get("compactionState"),
        Some(&serde_json::json!("cancelled"))
    );
}

#[test]
fn append_final_item_publishes_terminal_duration_from_backend_turn() {
    let session_id = SessionId::new("session-terminal-duration");
    let store = SessionStore::new();
    store
        .create_session(session_id.clone(), "terminal duration")
        .expect("session should be creatable");
    let (_mission_id, orchestrator_thread_id) =
        store.ensure_session_mission(&session_id, ts(1000), || {
            magi_core::MissionId::new("mission-terminal-duration")
        });
    let coordinator = SessionTurnCoordinator::new();
    seed_conversation_turn(
        &store,
        &session_id,
        &coordinator,
        ActiveExecutionTurn {
            turn_id: "turn-terminal-duration".to_string(),
            turn_seq: 1,
            accepted_at: ts(1000),
            completed_at: None,
            status: "running".to_string(),
            user_message: Some("请回答".to_string()),
            items: Vec::new(),
        },
    );
    let event_bus = InMemoryEventBus::new(16);
    let request = SessionTurnExecutionRequest {
        project_directories: None,
        session_id: session_id.clone(),
        turn_id: "turn-terminal-duration".to_string(),
        workspace_id: None,
        prompt: "请回答".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        request_id: Some("request-terminal-duration".to_string()),
        user_message_id: Some("user-terminal-duration".to_string()),
        placeholder_message_id: Some("placeholder-terminal-duration".to_string()),
        command: None,
    };

    append_final_item(
        TurnTerminalCommitPolicy::Executor,
        &event_bus,
        &store,
        &request,
        FinalItemInput {
            content: "最终回复",
            item_id: None,
            timeline_entry_id: None,
            model_round: None,
        },
        &orchestrator_thread_id,
        None,
    )
    .expect("final item should be stored and published");

    let terminal_event = event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .rev()
        .find(|event| event.event_type == "session.turn.item")
        .expect("terminal item event should be published");
    assert_eq!(
        terminal_event.payload["current_turn"]["status"],
        "completed"
    );
    assert!(
        terminal_event.payload["current_turn"]["response_duration_ms"]
            .as_u64()
            .is_some(),
        "terminal session.turn.item 必须携带后端完成耗时"
    );
    assert!(
        store
            .canonical_turns_for_session(&session_id)
            .iter()
            .any(|turn| turn.turn_id == "turn-terminal-duration"
                && turn.response_duration_ms.is_some()
                && turn.status == CanonicalTurnStatus::Completed),
        "持久 canonical turn 必须携带后端完成耗时"
    );
}
