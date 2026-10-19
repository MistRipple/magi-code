//! 两种原生入口共用的调用边界；恢复请求也必须先冻结，收到结果后才允许执行工具。
use magi_bridge_client::{
    BridgeClientError, BridgeErrorLayer, ModelBridgeClient, ModelInvocationRequest, ModelResponse,
    ModelRetryRuntimeEvent, ModelStreamingDelta,
};
use magi_core::{SessionId, TaskId, ThreadId, UtcMillis};
use magi_session_store::{
    ModelAttempt, ModelStep, ModelStepBinding, ModelStepStatus, SessionStore,
};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) struct StepModelClient<'a> {
    inner: &'a dyn ModelBridgeClient,
    store: &'a SessionStore,
    session_id: &'a SessionId,
    turn_id: Option<&'a str>,
    task_id: Option<&'a TaskId>,
    thread_id: &'a ThreadId,
    call_id: &'a str,
    sequence: AtomicUsize,
    binding: Option<ModelStepBinding>,
    purpose: magi_session_store::ModelStepPurpose,
    execution_scope: Option<magi_bridge_client::ModelExecutionScope>,
}

impl<'a> StepModelClient<'a> {
    pub fn new(
        inner: &'a dyn ModelBridgeClient,
        store: &'a SessionStore,
        session_id: &'a SessionId,
        turn_id: Option<&'a str>,
        task_id: Option<&'a TaskId>,
        thread_id: &'a ThreadId,
        call_id: &'a str,
    ) -> Self {
        Self {
            inner,
            store,
            session_id,
            turn_id,
            task_id,
            thread_id,
            call_id,
            sequence: AtomicUsize::new(0),
            binding: None,
            purpose: magi_session_store::ModelStepPurpose::Execution,
            execution_scope: None,
        }
    }

    pub fn with_purpose(mut self, purpose: magi_session_store::ModelStepPurpose) -> Self {
        self.purpose = purpose;
        self
    }

    pub fn with_execution_scope(mut self, scope: magi_bridge_client::ModelExecutionScope) -> Self {
        self.execution_scope = Some(scope);
        self
    }

    pub fn with_model_binding(
        mut self,
        settings: Option<&std::sync::Arc<magi_settings_store::SettingsStore>>,
        binding: &crate::usage_recording::ModelUsageBinding,
    ) -> Self {
        self.binding = Some(ModelStepBinding {
            model: crate::usage_recording::resolved_model_for_usage_binding(
                settings,
                binding,
                self.session_id,
            ),
            provider: crate::usage_recording::resolved_provider_for_usage_binding(
                settings,
                binding,
                self.session_id,
            ),
            binding_revision: binding.binding_revision(),
            settings_revision: settings.map_or(0, |settings| settings.revision()),
        });
        self
    }

    pub fn last_step_id(&self) -> Option<String> {
        match self.sequence.load(Ordering::SeqCst) {
            0 => None,
            1 => Some(self.call_id.to_string()),
            count => Some(format!("{}-recovery-{}", self.call_id, count - 1)),
        }
    }

    fn invoke_stream_recorded(
        &self,
        request: ModelInvocationRequest,
        delta: &dyn Fn(&ModelStreamingDelta),
        retry: &dyn Fn(&ModelRetryRuntimeEvent),
        cancelled: &dyn Fn() -> bool,
        limit: Option<u32>,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_recorded(
            request,
            retry,
            cancelled,
            limit,
            |client, request, retry, stopped, limit| {
                let on_delta = |value: &ModelStreamingDelta| {
                    if !stopped() {
                        delta(value);
                    }
                };
                match limit {
                    Some(limit) => client.invoke_streaming_with_output_limit(
                        request, limit, &on_delta, retry, stopped,
                    ),
                    None => client
                        .invoke_streaming_with_cancellation(request, &on_delta, retry, stopped),
                }
            },
        )
    }

    fn invoke_recorded(
        &self,
        request: ModelInvocationRequest,
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        cancelled: &dyn Fn() -> bool,
        max_output_tokens: Option<u32>,
        invoke: impl FnOnce(
            &dyn ModelBridgeClient,
            ModelInvocationRequest,
            &dyn Fn(&ModelRetryRuntimeEvent),
            &dyn Fn() -> bool,
            Option<u32>,
        ) -> Result<ModelResponse, BridgeClientError>,
    ) -> Result<ModelResponse, BridgeClientError> {
        if cancelled() {
            return Err(magi_bridge_client::model_invocation_cancelled_error());
        }
        // 缓存 client 不保存某个 Task 的权限。每次真实调用重新绑定本 Step 的可信作用域；
        // 外部工具循环必须确认绑定成功，不能把默认 Ok(None) 当作已继承权限。
        let bound = self
            .execution_scope
            .as_ref()
            .map(|scope| self.inner.bind_execution_scope(scope.clone()))
            .transpose()?
            .flatten();
        if cancelled() {
            return Err(magi_bridge_client::model_invocation_cancelled_error());
        }
        let client = bound.as_deref().unwrap_or(self.inner);
        let capabilities = client.executor_capabilities();
        if capabilities.tools == magi_bridge_client::ExecutorToolMode::ExecutorOwned
            && bound.is_none()
        {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32096),
                message:
                    "executor_scope_required: 外部执行器必须绑定本次执行目录及权限，调用尚未发送"
                        .into(),
            });
        }
        magi_bridge_client::validate_executor_request(&capabilities, &request)?;
        if max_output_tokens.is_some() && !capabilities.output_limit {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32097),
                message: "executor_output_limit_unsupported: 当前执行器不支持供应商输出上限，请切换执行器".into(),
            });
        }
        // ExecutorOwned 可能已经执行本地命令；没有 canonical Turn/Step 就没有
        // 可持久化的证据归属，失败后主线会把结果误判为“未执行”并重复唤起。
        // 原生模型仍允许无 Turn 的轻量调用（例如上下文辅助请求），外部执行器不允许。
        if self.turn_id.is_none()
            && capabilities.tools == magi_bridge_client::ExecutorToolMode::ExecutorOwned
        {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32096),
                message:
                    "executor_step_required: 外部执行器必须绑定 canonical Turn/ModelStep，调用尚未发送".into(),
            });
        }
        let Some(turn_id) = self.turn_id else {
            let response = invoke(client, request, on_retry, cancelled, max_output_tokens)?;
            magi_bridge_client::validate_executor_response(&capabilities, &response)?;
            return Ok(response);
        };
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst);
        let step_id = if sequence == 0 {
            self.call_id.to_string()
        } else {
            format!("{}-recovery-{sequence}", self.call_id)
        };
        // 消费证据来自本次实际冻结的消息，不能使用跨压缩/恢复累积的 ID 列表。
        // 历史中的其他 Turn 输入只作为上下文，压缩辅助请求也不能领取用户输入。
        let bound_inputs = self
            .store
            .input_request_ids_for_turn(self.session_id, turn_id);
        let mut input_request_ids =
            if self.purpose == magi_session_store::ModelStepPurpose::Execution {
                request
                    .messages
                    .iter()
                    .flatten()
                    .filter(|message| message.role == "user")
                    .filter_map(|message| message.input_request_id.as_ref())
                    .filter(|id| bound_inputs.contains(*id))
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };
        input_request_ids.sort();
        input_request_ids.dedup();
        let now = UtcMillis::now();
        let step = ModelStep {
            step_id: step_id.clone(),
            task_id: self.task_id.cloned(),
            thread_id: self.thread_id.clone(),
            attempts: vec![ModelAttempt {
                attempt_id: format!("{step_id}:attempt:1"),
                started_at: now,
                settled_at: None,
                status: ModelStepStatus::Started,
            }],
            goal_id: None,
            usage_tokens: None,
            goal_tokens_after: None,
            checkpoint_generation: self
                .store
                .thread_context_checkpoint(self.thread_id)
                .map_or(0, |checkpoint| checkpoint.generation),
            request_fingerprint: fingerprint(&serde_json::json!({
                "request": request, "maxOutputTokens": max_output_tokens,
                "executorCapabilities": capabilities, "purpose": self.purpose,
                "executionScope": self.execution_scope.as_ref().map(|scope| serde_json::json!({
                    "workingDir": scope.working_dir,
                    "policy": scope.policy,
                    "requiresNativeTools": scope.requires_native_tools,
                    "closedSkillScope": scope.closed_skill_scope,
                })),
            }))?,
            provider: request.provider.clone(),
            binding: self.binding.clone(),
            executor_capabilities: Some(capabilities.clone()),
            purpose: Some(self.purpose),
            message_count: request.messages.as_ref().map_or(0, Vec::len),
            tool_count: request.tools.as_ref().map_or(0, Vec::len),
            max_output_tokens,
            input_request_ids,
            started_at: now,
            settled_at: None,
            status: ModelStepStatus::Started,
            response_fingerprint: None,
        };
        self.store
            .begin_model_step(self.session_id, turn_id, step)
            .map_err(step_start_error)?;
        let retry_write_error = std::sync::Mutex::new(None::<magi_core::DomainError>);
        let recorded_retry = |event: &ModelRetryRuntimeEvent| {
            if event.phase == magi_bridge_client::ModelRetryRuntimePhase::AttemptStarted
                && let Err(error) =
                    self.store
                        .start_model_step_retry(self.session_id, turn_id, &step_id)
            {
                *retry_write_error.lock().expect("retry journal lock") = Some(error);
                return;
            }
            on_retry(event);
        };
        let stopped = || {
            cancelled()
                || retry_write_error
                    .lock()
                    .expect("retry journal lock")
                    .is_some()
        };
        let result = invoke(
            client,
            request,
            &recorded_retry,
            &stopped,
            max_output_tokens,
        );
        if let Some(error) = retry_write_error.into_inner().expect("retry journal lock") {
            return Err(step_start_error(error));
        }
        let response_fingerprint = result.as_ref().ok().map(fingerprint).transpose()?;
        let executor_failure = result
            .as_ref()
            .err()
            .and_then(|error| error.executor_failure_evidence());
        let usage = result
            .as_ref()
            .ok()
            .and_then(|response| response.usage.as_ref())
            .or_else(|| executor_failure.and_then(|evidence| evidence.usage.as_ref()));
        self.store
            .settle_model_step(
                self.session_id,
                turn_id,
                &step_id,
                response_fingerprint,
                crate::usage_recording::model_usage_tokens(usage),
                executor_failure,
            )
            .map_err(journal_error)?;
        // 允许迟到响应结算用量，但不能向已关闭的执行交付工具或正文。
        if cancelled()
            || self
                .store
                .canonical_turn_for_session_turn_id(self.session_id, turn_id)
                .is_some_and(|turn| turn.status.is_terminal() || turn.completed_at.is_some())
        {
            return Err(magi_bridge_client::model_invocation_cancelled_error());
        }
        if let Ok(response) = &result {
            magi_bridge_client::validate_executor_response(&capabilities, response)?;
        }
        result.map_err(|error| {
            let classification = crate::model_error::classify_model_invocation_error(&error.to_string());
            if capabilities.resume != magi_bridge_client::ExecutorResumeMode::Checkpoint
                && matches!(classification.code, "model_empty_response" | "model_stream_interrupted" | "model_context_limit")
            {
                BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::RemoteBusiness,
                    code: Some(-32096),
                    message: format!("executor_outcome_unknown: 执行器调用失败，未自动重发；请核对远端进展。{error}"),
                }
            } else {
                error
            }
        })
    }
}

impl ModelBridgeClient for StepModelClient<'_> {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        self.inner.executor_capabilities()
    }
    fn invoke(&self, request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_with_cancellation(request, &|| false)
    }
    fn invoke_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_with_retry_events_and_cancellation(request, &|_| {}, cancelled)
    }
    fn invoke_with_retry_events_and_cancellation(
        &self,
        request: ModelInvocationRequest,
        retry: &dyn Fn(&ModelRetryRuntimeEvent),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_recorded(
            request,
            retry,
            cancelled,
            None,
            |client, request, retry, stopped, limit| match limit {
                Some(limit) => client.invoke_with_retry_events_cancellation_and_output_limit(
                    request, limit, retry, stopped,
                ),
                None => client.invoke_with_retry_events_and_cancellation(request, retry, stopped),
            },
        )
    }
    fn invoke_with_cancellation_and_output_limit(
        &self,
        request: ModelInvocationRequest,
        limit: u32,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_with_retry_events_cancellation_and_output_limit(
            request,
            limit,
            &|_| {},
            cancelled,
        )
    }
    fn invoke_with_retry_events_cancellation_and_output_limit(
        &self,
        request: ModelInvocationRequest,
        limit: u32,
        retry: &dyn Fn(&ModelRetryRuntimeEvent),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_recorded(
            request,
            retry,
            cancelled,
            Some(limit),
            |client, request, retry, stopped, limit| {
                client.invoke_with_retry_events_cancellation_and_output_limit(
                    request,
                    limit.expect("explicit output limit"),
                    retry,
                    stopped,
                )
            },
        )
    }
    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_streaming_with_cancellation(request, delta, &|_| {}, &|| false)
    }
    fn invoke_streaming_with_retry_events(
        &self,
        request: ModelInvocationRequest,
        delta: &dyn Fn(&ModelStreamingDelta),
        retry: &dyn Fn(&ModelRetryRuntimeEvent),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_streaming_with_cancellation(request, delta, retry, &|| false)
    }
    fn invoke_streaming_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        delta: &dyn Fn(&ModelStreamingDelta),
        retry: &dyn Fn(&ModelRetryRuntimeEvent),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_stream_recorded(request, delta, retry, cancelled, None)
    }

    fn invoke_streaming_with_output_limit(
        &self,
        request: ModelInvocationRequest,
        limit: u32,
        delta: &dyn Fn(&ModelStreamingDelta),
        retry: &dyn Fn(&ModelRetryRuntimeEvent),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_stream_recorded(request, delta, retry, cancelled, Some(limit))
    }
}

fn step_start_error(error: magi_core::DomainError) -> BridgeClientError {
    if matches!(
        error,
        magi_core::DomainError::RunSafetyLimit { .. }
            | magi_core::DomainError::GoalRejected {
                reason: magi_core::GoalRejection::NotActive,
                ..
            }
    ) {
        return BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(-32098),
            message: error.to_string(),
        };
    }
    journal_error(error)
}

fn journal_error(error: impl std::fmt::Display) -> BridgeClientError {
    BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::RemoteBusiness,
        code: Some(-32099),
        message: format!("模型调用事实提交失败：{error}"),
    }
}
fn fingerprint(value: &impl serde::Serialize) -> Result<String, BridgeClientError> {
    // 转 Value 后按 key 排序，消除 HashMap 插入顺序对请求身份的影响。
    let value = serde_json::to_value(value).map_err(journal_error)?;
    let bytes = serde_json::to_vec(&value).map_err(journal_error)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(crate) fn root_input_request_id(
    store: &SessionStore,
    session_id: &SessionId,
    turn_id: &str,
) -> Option<String> {
    store
        .canonical_turn_for_session_turn_id(session_id, turn_id)
        .and_then(|turn| {
            turn.items
                .into_iter()
                .find(|item| item.kind == magi_session_store::CanonicalTurnItemKind::UserMessage)
        })
        .and_then(|item| {
            item.metadata
                .get("requestId")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{DomainError, DomainResult, Task};
    use magi_session_store::{
        ActiveExecutionTurn, CanonicalTurnMutation, SessionAcceptanceRecord, SessionEventWriter,
        SessionInputMutation, TimelineEntryInput, TimelineEntryKind,
    };
    use std::sync::Arc;

    fn store() -> (SessionStore, SessionId, ThreadId) {
        let store = SessionStore::new();
        let session = SessionId::new("session-journal-client");
        store
            .create_session(session.clone(), "journal client")
            .unwrap();
        store
            .accept_current_turn_with_timeline_entry(
                session.clone(),
                TimelineEntryInput::new(
                    "timeline",
                    TimelineEntryKind::UserMessage,
                    "work",
                    UtcMillis(1),
                ),
                ActiveExecutionTurn {
                    turn_id: "turn-journal-client".into(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(1),
                    status: "running".into(),
                    completed_at: None,
                    user_message: Some("work".into()),
                    items: vec![],
                },
            )
            .unwrap();
        (store, session, ThreadId::new("thread-journal-client"))
    }
    fn request(text: &str) -> ModelInvocationRequest {
        ModelInvocationRequest {
            provider: "test".into(),
            prompt: text.into(),
            messages: None,
            tools: None,
            tool_choice: None,
        }
    }

    #[test]
    fn only_original_inputs_in_the_frozen_request_are_consumed_after_restart() {
        use magi_session_store::{SessionInput, SessionInputState};
        let (store, session, thread) = store();
        for id in ["first", "same-text-later"] {
            store
                .commit_input(
                    SessionInput {
                        input_id: format!("input-{id}"),
                        session_id: session.clone(),
                        request_id: id.into(),
                        user_message_id: format!("user-{id}"),
                        request_fingerprint: id.into(),
                        accepted_at: UtcMillis(1),
                        revision: 1,
                        state: SessionInputState::Attached,
                        attached_turn_id: Some("turn-journal-client".into()),
                        consumption: None,
                        interruption: None,
                        payload: serde_json::json!({"text":"identical input"}),
                    },
                    None,
                )
                .unwrap();
        }
        let store = SessionStore::from_persisted_parts(
            store.durable_state(),
            store.execution_sidecar_store_state(),
        )
        .unwrap();
        let inner = Client {
            calls: AtomicUsize::new(0),
        };
        let client = StepModelClient::new(
            &inner,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "frozen",
        );
        let mut req = request("identical input");
        req.messages = Some(vec![magi_bridge_client::ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: "user".into(),
            content: Some("identical input".into()),
            images: vec![],
            tool_calls: vec![],
            tool_call_id: None,
            provider_context: vec![],
        }]);
        // 同文摘要没有原始身份，不能确认消费任意一条输入。
        client.invoke(req.clone()).unwrap();
        assert!(
            store
                .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
                .unwrap()
                .model_steps[0]
                .input_request_ids
                .is_empty()
        );
        for id in ["first", "same-text-later"] {
            assert_eq!(
                store.input_for_request(id).unwrap().state,
                SessionInputState::Attached
            );
        }
        req.messages.as_mut().unwrap()[0].input_request_id = Some("first".into());
        client.invoke(req.clone()).unwrap();
        assert_eq!(
            store.input_for_request("first").unwrap().state,
            SessionInputState::Consumed
        );
        assert_eq!(
            store.input_for_request("same-text-later").unwrap().state,
            SessionInputState::Attached
        );
        assert_eq!(
            store
                .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
                .unwrap()
                .model_steps[1]
                .input_request_ids,
            ["first"]
        );
        // 可信身份不会泄露到 Provider 的消息协议，也不会从外部 JSON 注入。
        let mut json = serde_json::to_value(&req).unwrap();
        assert!(json["messages"][0].get("input_request_id").is_none());
        json["messages"][0]["input_request_id"] = serde_json::json!("forged");
        json["messages"][0]["inputRequestId"] = serde_json::json!("forged");
        assert!(
            serde_json::from_value::<ModelInvocationRequest>(json)
                .unwrap()
                .messages
                .unwrap()[0]
                .input_request_id
                .is_none()
        );
        let compactor = StepModelClient::new(
            &inner,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "summary",
        )
        .with_purpose(magi_session_store::ModelStepPurpose::ContextCompaction);
        req.messages.as_mut().unwrap()[0].input_request_id = Some("same-text-later".into());
        compactor.invoke(req).unwrap();
        assert_eq!(
            store.input_for_request("same-text-later").unwrap().state,
            SessionInputState::Attached
        );
    }
    struct Client {
        calls: AtomicUsize,
    }

    struct TextExecutor {
        calls: AtomicUsize,
        unexpected_tool: bool,
    }

    struct UnscopedExecutor;

    impl ModelBridgeClient for UnscopedExecutor {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            let mut caps = magi_bridge_client::text_executor_capabilities();
            caps.tools = magi_bridge_client::ExecutorToolMode::ExecutorOwned;
            caps
        }

        fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
            panic!("未实现作用域绑定的执行器不得收到请求")
        }

        fn invoke_streaming(
            &self,
            _: ModelInvocationRequest,
            _: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            panic!("未实现作用域绑定的执行器不得收到流式请求")
        }
    }

    #[test]
    fn default_scope_binding_is_not_an_executor_authorization() {
        let (store, session, thread) = store();
        let client = StepModelClient::new(
            &UnscopedExecutor,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "unscoped",
        )
        .with_execution_scope(external_scope("/first"));
        for error in [
            client.invoke(request("work")).unwrap_err(),
            client
                .invoke_streaming(request("work"), &|_| {})
                .unwrap_err(),
        ] {
            assert!(error.to_string().contains("executor_scope_required"));
        }
        assert!(
            store
                .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
                .unwrap()
                .model_steps
                .is_empty()
        );
    }

    #[derive(Clone)]
    struct ScopedExecutor {
        bindings: Arc<std::sync::Mutex<Vec<magi_bridge_client::ModelExecutionScope>>>,
        calls: Arc<AtomicUsize>,
        bound: bool,
        reject_binding: bool,
    }

    impl ModelBridgeClient for ScopedExecutor {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            let mut caps = magi_bridge_client::text_executor_capabilities();
            caps.tools = magi_bridge_client::ExecutorToolMode::ExecutorOwned;
            caps
        }
        fn bind_execution_scope(
            &self,
            scope: magi_bridge_client::ModelExecutionScope,
        ) -> Result<Option<Arc<dyn ModelBridgeClient>>, BridgeClientError> {
            self.bindings.lock().unwrap().push(scope);
            if self.reject_binding {
                return Err(BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::RemoteBusiness,
                    code: Some(-32096),
                    message: "executor_policy_unsupported: test policy rejected".into(),
                });
            }
            let mut client = self.clone();
            client.bound = true;
            Ok(Some(Arc::new(client)))
        }
        fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
            assert!(self.bound, "不能调用未绑定权限的缓存实例");
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ModelResponse::completed("external result"))
        }
        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    fn external_scope(root: &str) -> magi_bridge_client::ModelExecutionScope {
        magi_bridge_client::ModelExecutionScope {
            working_dir: Some(root.into()),
            policy: crate::agent_spawn_preflight::agent_spawn_child_policy_snapshot(None),
            requires_native_tools: false,
            closed_skill_scope: false,
        }
    }

    #[test]
    fn external_executor_is_rebound_per_invocation_and_scope_changes_the_request_identity() {
        let (store, session, thread) = store();
        let inner = ScopedExecutor {
            bindings: Default::default(),
            calls: Default::default(),
            bound: false,
            reject_binding: false,
        };
        let client = StepModelClient::new(
            &inner,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "external",
        );
        assert!(
            client
                .invoke(request("work"))
                .unwrap_err()
                .to_string()
                .contains("executor_scope_required")
        );
        assert!(inner.bindings.lock().unwrap().is_empty());
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        let client = client.with_execution_scope(external_scope("/first"));
        client.invoke(request("work")).unwrap();
        client.invoke_streaming(request("work"), &|_| {}).unwrap();
        let client = client.with_execution_scope(external_scope("/second"));
        client.invoke(request("work")).unwrap();
        let bindings = inner.bindings.lock().unwrap();
        assert_eq!(bindings.len(), 3);
        assert_eq!(bindings[0].working_dir, Some("/first".into()));
        assert_eq!(bindings[2].working_dir, Some("/second".into()));
        let turn = store
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap();
        assert_eq!(turn.model_steps.len(), 3);
        assert_eq!(
            turn.model_steps[0].request_fingerprint,
            turn.model_steps[1].request_fingerprint
        );
        assert_ne!(
            turn.model_steps[0].request_fingerprint,
            turn.model_steps[2].request_fingerprint
        );
        assert!(!inner.bound);
    }

    #[test]
    fn external_executor_without_a_canonical_turn_is_rejected_before_send() {
        let (store, session, thread) = store();
        let inner = ScopedExecutor {
            bindings: Default::default(),
            calls: Default::default(),
            bound: false,
            reject_binding: false,
        };
        let client = StepModelClient::new(
            &inner,
            &store,
            &session,
            None,
            None,
            &thread,
            "untracked-external",
        )
        .with_execution_scope(external_scope("/workspace"));
        let error = client.invoke(request("inspect")).unwrap_err();
        assert!(error.to_string().contains("executor_step_required"));
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        assert!(
            store
                .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
                .unwrap()
                .model_steps
                .is_empty()
        );
    }

    #[test]
    fn binding_rejection_and_cancellation_do_not_create_step_or_invoke_executor() {
        let (store, session, thread) = store();
        let inner = ScopedExecutor {
            bindings: Default::default(),
            calls: Default::default(),
            bound: false,
            reject_binding: true,
        };
        let client = StepModelClient::new(
            &inner,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "denied-external",
        )
        .with_execution_scope(external_scope("/first"));
        assert!(
            client
                .invoke(request("work"))
                .unwrap_err()
                .to_string()
                .contains("policy_unsupported")
        );
        assert!(
            client
                .invoke_with_cancellation(request("work"), &|| true)
                .is_err()
        );
        assert_eq!(inner.bindings.lock().unwrap().len(), 1);
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        assert!(
            store
                .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
                .unwrap()
                .model_steps
                .is_empty()
        );
    }

    impl ModelBridgeClient for TextExecutor {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            magi_bridge_client::text_executor_capabilities()
        }
        fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut response = ModelResponse::completed("done");
            if self.unexpected_tool {
                response.tool_calls.push(magi_bridge_client::ChatToolCall {
                    id: "unexpected".into(),
                    kind: "function".into(),
                    function: magi_bridge_client::ChatToolFunction {
                        name: "shell_exec".into(),
                        arguments: r#"{"command":"publish"}"#.into(),
                    },
                });
            }
            Ok(response)
        }
        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    #[test]
    fn executor_contract_rejects_before_send_and_freezes_received_response_before_refusing_tools() {
        let (store, session, thread) = store();
        let inner = TextExecutor {
            calls: AtomicUsize::new(0),
            unexpected_tool: false,
        };
        let client = StepModelClient::new(
            &inner,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "caps",
        );
        let mut unsupported = request("unsupported");
        unsupported.tool_choice = Some(magi_bridge_client::ChatToolChoice::force_function(
            "shell_exec",
        ));
        assert!(
            client
                .invoke(unsupported)
                .unwrap_err()
                .to_string()
                .contains("executor_native_tools_unsupported")
        );
        assert!(
            client
                .invoke_with_cancellation_and_output_limit(request("limited"), 100, &|| false)
                .unwrap_err()
                .to_string()
                .contains("executor_output_limit_unsupported")
        );
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        assert!(
            store
                .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
                .unwrap()
                .model_steps
                .is_empty()
        );
        client.invoke(request("supported")).unwrap();
        let turn = store
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap();
        assert_eq!(
            turn.model_steps[0].executor_capabilities,
            Some(inner.executor_capabilities())
        );
        let reloaded = SessionStore::from_persisted_parts(
            store.durable_state(),
            store.execution_sidecar_store_state(),
        )
        .unwrap();
        assert_eq!(
            reloaded
                .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
                .unwrap()
                .model_steps[0]
                .executor_capabilities,
            Some(inner.executor_capabilities())
        );
        let bad = TextExecutor {
            calls: AtomicUsize::new(0),
            unexpected_tool: true,
        };
        let client = StepModelClient::new(
            &bad,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "bad-caps",
        );
        assert!(
            client
                .invoke(request("bad response"))
                .unwrap_err()
                .to_string()
                .contains("executor_native_tools_unsupported")
        );
        let turn = store
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap();
        assert_eq!(
            turn.model_steps[1].status,
            magi_session_store::ModelStepStatus::ResponseReceived
        );
        assert!(turn.model_steps[1].response_fingerprint.is_some());
    }

    impl ModelBridgeClient for Client {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            magi_bridge_client::native_executor_capabilities()
        }

        fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ModelResponse::completed("done"))
        }
        fn invoke_streaming(
            &self,
            _: ModelInvocationRequest,
            _: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Transport,
                code: Some(-32005),
                message: "stream interrupted".into(),
            })
        }
    }
    struct RejectWrites;
    impl SessionEventWriter for RejectWrites {
        fn append_session_transaction(
            &self,
            _: &SessionId,
            _: &[CanonicalTurnMutation],
            _: &[SessionInputMutation],
            _: Option<(&SessionAcceptanceRecord, Option<&Task>)>,
        ) -> DomainResult<()> {
            Err(DomainError::Persistence {
                message: "injected".into(),
            })
        }
    }
    #[test]
    fn changed_recovery_request_has_its_own_identity_and_unknown_stream_is_preserved() {
        let (store, session, thread) = store();
        let inner = Client {
            calls: AtomicUsize::new(0),
        };
        let client = StepModelClient::new(
            &inner,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "call-0",
        );
        assert!(
            client
                .invoke_streaming(request("original"), &|_| {})
                .is_err()
        );
        client
            .invoke_with_cancellation(request("recover with partial output"), &|| false)
            .unwrap();
        let turn = store
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap();
        assert_eq!(turn.model_steps.len(), 2);
        assert_eq!(turn.model_steps[0].status, ModelStepStatus::OutcomeUnknown);
        assert_eq!(
            turn.model_steps[1].status,
            ModelStepStatus::ResponseReceived
        );
        assert_ne!(
            turn.model_steps[0].request_fingerprint,
            turn.model_steps[1].request_fingerprint
        );
        assert_ne!(turn.model_steps[0].step_id, turn.model_steps[1].step_id);
        assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    }
    struct RetryClient<'a> {
        store: &'a SessionStore,
        reject_retry: bool,
    }
    impl ModelBridgeClient for RetryClient<'_> {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            magi_bridge_client::native_executor_capabilities()
        }

        fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
            unreachable!()
        }
        fn invoke_streaming(
            &self,
            _: ModelInvocationRequest,
            _: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            unreachable!()
        }
        fn invoke_with_retry_events_and_cancellation(
            &self,
            _: ModelInvocationRequest,
            retry: &dyn Fn(&ModelRetryRuntimeEvent),
            cancelled: &dyn Fn() -> bool,
        ) -> Result<ModelResponse, BridgeClientError> {
            if self.reject_retry {
                self.store
                    .install_session_event_writer(Arc::new(RejectWrites));
            }
            retry(&ModelRetryRuntimeEvent {
                phase: magi_bridge_client::ModelRetryRuntimePhase::AttemptStarted,
                attempt: 1,
                max_attempts: 2,
                delay_ms: None,
            });
            if cancelled() {
                return Err(magi_bridge_client::model_invocation_cancelled_error());
            }
            Ok(ModelResponse::completed("after retry"))
        }
    }
    #[test]
    fn transport_retry_is_durable_and_failure_to_record_it_cancels_before_resend() {
        for reject_retry in [false, true] {
            let (store, session, thread) = store();
            let inner = RetryClient {
                store: &store,
                reject_retry,
            };
            let client = StepModelClient::new(
                &inner,
                &store,
                &session,
                Some("turn-journal-client"),
                None,
                &thread,
                "call-retry",
            );
            let result = client.invoke(request("retry"));
            assert_eq!(result.is_err(), reject_retry);
            let turn = store
                .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
                .unwrap();
            let step = &turn.model_steps[0];
            if reject_retry {
                assert_eq!(step.attempts.len(), 1);
                assert_eq!(step.status, ModelStepStatus::Started);
            } else {
                assert_eq!(step.attempts.len(), 2);
                assert_eq!(step.attempts[0].status, ModelStepStatus::OutcomeUnknown);
                assert_eq!(step.attempts[1].status, ModelStepStatus::ResponseReceived);
            }
        }
    }

    #[test]
    fn persistence_failure_prevents_provider_call_and_no_input_is_required_for_a_step() {
        let (store, session, thread) = store();
        let inner = Client {
            calls: AtomicUsize::new(0),
        };
        let client = StepModelClient::new(
            &inner,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "call-0",
        );
        store.install_session_event_writer(Arc::new(RejectWrites));
        assert!(client.invoke(request("original")).is_err());
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        assert!(
            store
                .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
                .unwrap()
                .model_steps
                .is_empty()
        );
    }
    #[test]
    fn cancelled_turn_settles_late_usage_without_delivering_the_response() {
        let (store, session, thread) = store();
        let (_, main_thread) = store.ensure_session_mission(&session, UtcMillis::now(), || {
            magi_core::MissionId::new("mission-late-usage")
        });
        store
            .create_goal(
                session.clone(),
                main_thread,
                "turn-journal-client",
                "usage accounting",
                magi_core::AccessProfile::Restricted,
            )
            .unwrap();
        struct LateClient<'a> {
            store: &'a SessionStore,
            session: &'a SessionId,
        }
        impl ModelBridgeClient for LateClient<'_> {
            fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
                magi_bridge_client::native_executor_capabilities()
            }

            fn invoke(
                &self,
                _: ModelInvocationRequest,
            ) -> Result<ModelResponse, BridgeClientError> {
                self.store
                    .update_current_turn_status_for_turn_with_change(
                        self.session,
                        Some("turn-journal-client"),
                        "cancelled",
                    )
                    .unwrap();
                let mut response = ModelResponse::completed("late");
                response.usage =
                    Some(serde_json::json!({"prompt_tokens":5,"completion_tokens":5008}));
                Ok(response)
            }
            fn invoke_streaming(
                &self,
                request: ModelInvocationRequest,
                _: &dyn Fn(&ModelStreamingDelta),
            ) -> Result<ModelResponse, BridgeClientError> {
                self.invoke(request)
            }
        }
        let inner = LateClient {
            store: &store,
            session: &session,
        };
        let client = StepModelClient::new(
            &inner,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "late-call",
        );
        assert!(
            client.invoke(request("work")).is_err(),
            "迟到模型响应不能再次交付"
        );
        let goal = store.current_goal(&session).unwrap();
        assert_eq!(goal.tokens_used, 5013);
        let restored = SessionStore::from_persisted_parts(
            store.durable_state(),
            store.execution_sidecar_store_state(),
        )
        .unwrap();
        assert_eq!(restored.current_goal(&session).unwrap().tokens_used, 5013);
        let step = &restored
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap()
            .model_steps[0];
        assert_eq!(step.status, ModelStepStatus::ResponseReceived);
        assert_eq!(step.usage_tokens, Some(5013));
    }

    #[test]
    fn user_pause_blocks_new_requests_before_turn_cancellation_finishes_and_resume_reopens_it() {
        let (store, session, thread) = store();
        let (_, main_thread) = store.ensure_session_mission(&session, UtcMillis::now(), || {
            magi_core::MissionId::new("mission-pause-before-cancel")
        });
        let goal = store
            .create_goal(
                session.clone(),
                main_thread,
                "turn-journal-client",
                "pause",
                magi_core::AccessProfile::Restricted,
            )
            .unwrap();
        // 先冻结一个已发送请求，暂停必须同时阻止它的 transport 重试。
        let now = UtcMillis::now();
        store
            .begin_model_step(
                &session,
                "turn-journal-client",
                ModelStep {
                    step_id: "before-pause".into(),
                    attempts: vec![ModelAttempt {
                        attempt_id: "before-pause:attempt:1".into(),
                        started_at: now,
                        settled_at: None,
                        status: ModelStepStatus::Started,
                    }],
                    task_id: None,
                    goal_id: None,
                    usage_tokens: None,
                    goal_tokens_after: None,
                    thread_id: thread.clone(),
                    checkpoint_generation: 0,
                    request_fingerprint: "a".repeat(64),
                    provider: "test".into(),
                    binding: None,
                    executor_capabilities: None,
                    purpose: None,
                    message_count: 1,
                    tool_count: 0,
                    max_output_tokens: None,
                    input_request_ids: vec![],
                    started_at: now,
                    settled_at: None,
                    status: ModelStepStatus::Started,
                    response_fingerprint: None,
                },
            )
            .unwrap();
        let paused = store
            .pause_goal_with_plan(&session, &goal.goal_id, goal.control_revision, None)
            .unwrap()
            .0;
        let inner = Client {
            calls: AtomicUsize::new(0),
        };
        let error = StepModelClient::new(
            &inner,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "paused",
        )
        .invoke(request("do not send"))
        .unwrap_err();
        assert_eq!(
            crate::model_error::classify_model_invocation_error(&error.to_string()).code,
            "run_paused"
        );
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        assert!(
            store
                .start_model_step_retry(&session, "turn-journal-client", "before-pause")
                .is_err()
        );
        let steps = store
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap()
            .model_steps;
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].attempts.len(), 1);
        store
            .resume_goal_with_plan(
                &session,
                &paused.goal_id,
                paused.control_revision,
                None,
                None,
            )
            .unwrap();
        StepModelClient::new(
            &inner,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "resumed",
        )
        .invoke(request("resume"))
        .unwrap();
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    }
}
