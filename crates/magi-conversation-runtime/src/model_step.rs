//! 两种原生入口共用的调用边界；恢复请求也必须先冻结，收到结果后才允许执行工具。
use crate::prompt_lineage::{PromptBranchScope, PromptFrame};
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

pub struct StepModelClient<'a> {
    inner: &'a dyn ModelBridgeClient,
    store: &'a SessionStore,
    session_id: &'a SessionId,
    turn_id: Option<&'a str>,
    task_id: Option<&'a TaskId>,
    thread_id: &'a ThreadId,
    call_id: &'a str,
    history_generation: u64,
    sequence: AtomicUsize,
    binding: Option<ModelStepBinding>,
    purpose: magi_session_store::ModelStepPurpose,
    prompt_branch: Option<PromptBranchScope>,
    execution_scope: Option<magi_bridge_client::ModelExecutionScope>,
    history_projection: Option<magi_session_store::HistoryProjection>,
    /// 扩展观察钩子（`model.resolved` / `model.request` / `model.response`）。
    extension_hooks: Option<&'a dyn crate::extension_hooks::ExtensionHooks>,
}

impl<'a> StepModelClient<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        inner: &'a dyn ModelBridgeClient,
        store: &'a SessionStore,
        session_id: &'a SessionId,
        turn_id: Option<&'a str>,
        task_id: Option<&'a TaskId>,
        thread_id: &'a ThreadId,
        call_id: &'a str,
        history_generation: u64,
    ) -> Self {
        Self {
            inner,
            store,
            session_id,
            turn_id,
            task_id,
            thread_id,
            call_id,
            history_generation,
            sequence: AtomicUsize::new(0),
            binding: None,
            purpose: magi_session_store::ModelStepPurpose::Execution,
            prompt_branch: None,
            execution_scope: None,
            history_projection: None,
            extension_hooks: None,
        }
    }

    pub fn with_extension_hooks(
        mut self,
        hooks: Option<&'a dyn crate::extension_hooks::ExtensionHooks>,
    ) -> Self {
        self.extension_hooks = hooks;
        self
    }

    pub fn with_purpose(mut self, purpose: magi_session_store::ModelStepPurpose) -> Self {
        self.purpose = purpose;
        self
    }

    pub fn with_prompt_branch(mut self, branch: PromptBranchScope) -> Self {
        self.prompt_branch = Some(branch);
        self
    }

    /// 本次请求视图对已有历史的改写量；缓存断代据此区分“有意改写”和未声明的改写。
    pub fn with_history_projection(
        mut self,
        projection: magi_session_store::HistoryProjection,
    ) -> Self {
        self.history_projection = Some(projection);
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
            usage_context: crate::usage_recording::model_step_usage_context(
                settings,
                binding,
                self.session_id,
            ),
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

    pub fn with_usage_assignment(mut self, assignment_id: String) -> Self {
        if let Some(context) = self
            .binding
            .as_mut()
            .and_then(|binding| binding.usage_context.as_mut())
        {
            context.assignment_id = Some(assignment_id);
        }
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
            Some(delta),
            retry,
            cancelled,
            limit,
            |client, request, retry, stopped, limit, observer| {
                let on_delta = |value: &ModelStreamingDelta| {
                    if !stopped() {
                        delta(value);
                    }
                };
                client.invoke_observed(
                    request,
                    magi_bridge_client::ModelInvocationOptions {
                        on_delta: Some(&on_delta),
                        on_retry: retry,
                        is_cancelled: stopped,
                        max_output_tokens: limit,
                    },
                    observer,
                )
            },
        )
    }

    /// 把本次请求的缓存表现拆成“Magi 保持稳定的前缀”“每次重发的尾部”和“provider 未服务的部分”。
    /// 只写日志，不新增持久事实：诊断时与 Step 里的尺寸记录和用量对照即可复算。
    fn log_cache_reuse(
        &self,
        step_id: &str,
        previous: Option<&magi_session_store::ModelPromptFrame>,
        current: &magi_session_store::ModelPromptFrame,
        usage: Option<&magi_app_server_protocol::ModelTransportUsage>,
    ) {
        let Some(usage) = usage else {
            return;
        };
        let read = usage.cache_read_tokens.unwrap_or(0);
        let input = usage.input_tokens
            + if usage.cache_read_included_in_input {
                0
            } else {
                read
            };
        if let Some(account) =
            crate::prompt_lineage::cache_reuse_account(previous, current, input, read)
        {
            tracing::info!(
                target: "magi::prompt_cache",
                step_id,
                input_tokens = input,
                cache_read_tokens = read,
                expected_cached_tokens = account.expected_cached_tokens,
                volatile_tail_tokens = account.volatile_tail_tokens,
                provider_shortfall_tokens = account.provider_shortfall_tokens,
                "缓存核算"
            );
        }
    }

    fn invoke_recorded(
        &self,
        mut request: ModelInvocationRequest,
        on_delta: Option<&dyn Fn(&ModelStreamingDelta)>,
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        cancelled: &dyn Fn() -> bool,
        max_output_tokens: Option<u32>,
        invoke: impl Fn(
            &dyn ModelBridgeClient,
            ModelInvocationRequest,
            &dyn Fn(&ModelRetryRuntimeEvent),
            &dyn Fn() -> bool,
            Option<u32>,
            &magi_bridge_client::ModelTransportObserver<'_>,
        ) -> Result<ModelResponse, BridgeClientError>,
    ) -> Result<ModelResponse, BridgeClientError> {
        const MAX_PLUGIN_LLM_STREAM_RETRIES: u32 = 3;
        let mut plugin_attempt = 0u32;
        loop {
            let mut response = self.invoke_recorded_once(
                request.clone(),
                on_retry,
                cancelled,
                max_output_tokens,
                &invoke,
            )?;
            let Some(hooks) = self.extension_hooks else {
                return Ok(response);
            };
            let tool_call_names: Vec<String> = response
                .tool_calls
                .iter()
                .map(|call| call.function.name.clone())
                .collect();
            let scope = crate::extension_hooks::ToolHookScope {
                session_id: self.session_id.as_str(),
                task_id: self.task_id.map_or("", |id| id.as_str()),
                workspace_id: None,
                worker_id: None,
                round: self.sequence.load(Ordering::SeqCst),
            };
            let view = crate::extension_hooks::LlmStreamInterceptView {
                provider: &request.provider,
                model: self
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.model.as_deref()),
                attempt: plugin_attempt,
                content: response.content.as_deref().unwrap_or(""),
                reasoning_content: response.thinking.as_deref(),
                tool_call_names: &tool_call_names,
                finish_reason: response.finish_reason.as_deref(),
            };
            match hooks.intercept_llm_stream(&scope, &view) {
                crate::extension_hooks::LlmStreamInterceptDecision::Accept => return Ok(response),
                crate::extension_hooks::LlmStreamInterceptDecision::Replace { content } => {
                    if let Some(delta_cb) = on_delta {
                        delta_cb(&ModelStreamingDelta {
                            content: content.clone(),
                            thinking: response.thinking.clone().unwrap_or_default(),
                            tool_calls: response.tool_calls.clone(),
                            replace: true,
                        });
                    }
                    response.content = Some(content);
                    return Ok(response);
                }
                crate::extension_hooks::LlmStreamInterceptDecision::Retry {
                    reason,
                    append_user_message,
                } => {
                    if plugin_attempt >= MAX_PLUGIN_LLM_STREAM_RETRIES || cancelled() {
                        tracing::warn!(
                            session_id = %self.session_id,
                            attempt = plugin_attempt,
                            reason = reason.as_deref().unwrap_or(""),
                            "llm/stream 环绕重试已达到上限或已取消，按当前响应继续"
                        );
                        return Ok(response);
                    }
                    if let Some(delta_cb) = on_delta {
                        delta_cb(&ModelStreamingDelta {
                            content: String::new(),
                            thinking: String::new(),
                            tool_calls: Vec::new(),
                            replace: true,
                        });
                    }
                    plugin_attempt += 1;
                    tracing::info!(
                        session_id = %self.session_id,
                        attempt = plugin_attempt,
                        reason = reason.as_deref().unwrap_or(""),
                        "llm/stream 环绕插件触发有界重试"
                    );
                    if let Some(text) = append_user_message.filter(|t| !t.trim().is_empty()) {
                        let target = request.messages.get_or_insert_with(Vec::new);
                        target.push(magi_bridge_client::ChatMessage {
                            input_request_id: None,
                            context_origin: magi_bridge_client::ChatMessageOrigin::ContextReference,
                            role: "user".to_string(),
                            content: Some(text),
                            images: Vec::new(),
                            tool_calls: Vec::new(),
                            tool_call_id: None,
                            provider_context: Vec::new(),
                        });
                    }
                }
            }
        }
    }

    fn invoke_recorded_once(
        &self,
        mut request: ModelInvocationRequest,
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        cancelled: &dyn Fn() -> bool,
        mut max_output_tokens: Option<u32>,
        invoke: &impl Fn(
            &dyn ModelBridgeClient,
            ModelInvocationRequest,
            &dyn Fn(&ModelRetryRuntimeEvent),
            &dyn Fn() -> bool,
            Option<u32>,
            &magi_bridge_client::ModelTransportObserver<'_>,
        ) -> Result<ModelResponse, BridgeClientError>,
    ) -> Result<ModelResponse, BridgeClientError> {
        if cancelled() {
            return Err(magi_bridge_client::model_invocation_cancelled_error());
        }
        if let Some(hooks) = self.extension_hooks {
            let scope = crate::extension_hooks::ToolHookScope {
                session_id: self.session_id.as_str(),
                task_id: self.task_id.map_or("", |id| id.as_str()),
                workspace_id: None,
                worker_id: None,
                round: self.sequence.load(Ordering::SeqCst),
            };
            let view = crate::extension_hooks::LlmRequestExtendView {
                provider: &request.provider,
                model: self
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.model.as_deref()),
                max_output_tokens,
                message_count: request.messages.as_ref().map_or(0, Vec::len),
                tool_count: request.tools.as_ref().map_or(0, Vec::len),
            };
            if let Some(ext) = hooks.extend_llm_request(&scope, &view) {
                if let Some(limit) = ext.max_output_tokens {
                    max_output_tokens = Some(limit);
                }
                if !ext.append_user_messages.is_empty() {
                    let target = request.messages.get_or_insert_with(Vec::new);
                    for text in ext.append_user_messages {
                        if !text.trim().is_empty() {
                            target.push(magi_bridge_client::ChatMessage {
                                input_request_id: None,
                                context_origin:
                                    magi_bridge_client::ChatMessageOrigin::ContextReference,
                                role: "user".to_string(),
                                content: Some(text),
                                images: Vec::new(),
                                tool_calls: Vec::new(),
                                tool_call_id: None,
                                provider_context: Vec::new(),
                            });
                        }
                    }
                }
            }
        }
        let branch = self.prompt_branch.clone().unwrap_or_else(|| {
            if self.purpose == magi_session_store::ModelStepPurpose::ContextCompaction {
                PromptBranchScope::Auxiliary {
                    purpose: "context_compaction".to_string(),
                }
            } else {
                PromptBranchScope::Orchestrator
            }
        });
        // 身份与组装好的历史一起冻结；发送时不能重新读取可能已变化的检查点。
        let history_generation = self.history_generation;
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
        // 缓存摘要必须读取与实际 Provider renderer 相同的规范化 IR。HTTP
        // client 会在这里完成工具 wire name 编码和能力降级；不能基于原始
        // canonical 工具名先生成一份与真实请求不同的缓存身份。
        let prompt_ir = client.cache_prompt_ir(&request);
        let mut prompt_frame = PromptFrame::from_ir(&prompt_ir);
        prompt_frame.history_projection = self.history_projection.clone();
        let capabilities = client.executor_capabilities();
        let provider_scope = crate::prompt_lineage::ProviderCacheScope::from_client(client)
            .unwrap_or_else(|| crate::prompt_lineage::ProviderCacheScope {
                provider: self
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.provider.clone())
                    .or_else(|| Some(request.provider.clone())),
                resolved_model: self
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.model.clone()),
                ..Default::default()
            });
        let lineage = crate::prompt_lineage::PromptLineage::for_branch(self.session_id, branch)
            .with_provider_scope(provider_scope);
        let previous = self
            .store
            .latest_model_step_for_cache_lineage(self.session_id, &lineage.lineage_id);
        let (lineage, cache_binding) = prompt_frame.bind_lineage(
            lineage,
            history_generation,
            self.execution_scope.as_ref(),
            previous.as_ref(),
        );
        let cache_epoch = lineage.cache_epoch;
        request.cache_affinity_key = Some(lineage.lineage_id);
        request.prompt_cache_key = Some(cache_binding.cache_key.clone());

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
            let response = invoke(
                client,
                request,
                on_retry,
                cancelled,
                max_output_tokens,
                &|_| Ok(()),
            )?;
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
        let mut persisted_frame: magi_session_store::ModelPromptFrame = (&prompt_frame).into();
        persisted_frame.cache_binding = Some(cache_binding);
        let frame_for_account = persisted_frame.clone();
        let step = ModelStep {
            step_id: step_id.clone(),
            task_id: self.task_id.cloned(),
            thread_id: self.thread_id.clone(),
            attempts: vec![ModelAttempt {
                transport: None,
                attempt_id: format!("{step_id}:attempt:1"),
                started_at: now,
                settled_at: None,
                status: ModelStepStatus::Started,
            }],
            goal_id: None,
            usage_tokens: None,
            goal_tokens_after: None,
            checkpoint_generation: history_generation,
            request_fingerprint: fingerprint(&serde_json::json!({
                "request": request, "maxOutputTokens": max_output_tokens,
                "executorCapabilities": capabilities, "purpose": self.purpose,
                "promptFrame": prompt_frame,
                "cacheEpoch": cache_epoch,
                "historyGeneration": history_generation,
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
            prompt_frame: Some(persisted_frame),
            cache_epoch,
        };
        // 观察通知在 Step 事实提交之后发出，且只带有限字段（不含提示词、消息正文或凭据）。
        let observed = self.extension_hooks.map(|hooks| {
            (
                hooks,
                serde_json::json!({
                    "sessionId": self.session_id.as_str(),
                    "turnId": turn_id,
                    "stepId": step_id,
                    "purpose": self.purpose,
                }),
                serde_json::json!({
                    "provider": step.provider,
                    "model": step.binding.as_ref().and_then(|binding| binding.model.clone()),
                    "messageCount": step.message_count,
                    "toolCount": step.tool_count,
                    "maxOutputTokens": step.max_output_tokens,
                    "requestFingerprint": step.request_fingerprint,
                    "cacheEpoch": step.cache_epoch,
                }),
            )
        });
        let request_snapshot = build_model_request_snapshot(
            &step,
            &request,
            &prompt_ir,
            &capabilities,
            max_output_tokens,
        );
        self.store
            .begin_model_step(self.session_id, turn_id, step)
            .map_err(step_start_error)?;
        if let Some(snapshot) = request_snapshot
            && let Err(error) = self
                .store
                .record_model_request(self.session_id, turn_id, snapshot)
        {
            tracing::warn!(
                session_id = %self.session_id,
                turn_id,
                step_id = %step_id,
                error = %error,
                "模型请求记录写入失败，历史检视将缺少该步骤的请求头与系统提示"
            );
        }
        if let Some((hooks, identity, request_view)) = &observed {
            let merged = |extra: &serde_json::Value| {
                let mut value = identity.clone();
                if let (Some(target), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
                    target.extend(extra.clone());
                }
                value
            };
            hooks.observe(
                "model.resolved",
                merged(&serde_json::json!({
                    "provider": request_view["provider"],
                    "model": request_view["model"],
                })),
            );
            hooks.observe("model.request", merged(request_view));
        }
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
        let transport_record =
            std::sync::Mutex::new(magi_bridge_client::ModelTransportRecorder::default());
        let stream_recorded = std::sync::atomic::AtomicBool::new(false);
        let on_transport = |event: &magi_bridge_client::ModelTransportEvent| {
            // 非持久化实时助手流：只转发给在线观察者，不落盘、不进传输回执。
            match event {
                magi_bridge_client::ModelTransportEvent::LiveStreamStart => {
                    self.store.publish_live_assistant_stream(
                        self.session_id,
                        turn_id,
                        &step_id,
                        magi_session_store::LiveAssistantStreamEvent::Start,
                    );
                    return Ok(());
                }
                magi_bridge_client::ModelTransportEvent::LiveStreamChunk(chunk) => {
                    self.store.publish_live_assistant_stream(
                        self.session_id,
                        turn_id,
                        &step_id,
                        magi_session_store::LiveAssistantStreamEvent::Chunk {
                            time: chunk.time,
                            chunk: chunk.chunk.clone(),
                        },
                    );
                    return Ok(());
                }
                magi_bridge_client::ModelTransportEvent::LiveStreamAbandon => {
                    self.store.publish_live_assistant_stream(
                        self.session_id,
                        turn_id,
                        &step_id,
                        magi_session_store::LiveAssistantStreamEvent::Abandon,
                    );
                    return Ok(());
                }
                magi_bridge_client::ModelTransportEvent::StreamRecorded(stream) => {
                    stream_recorded.store(true, Ordering::Release);
                    // 流记录不属于传输回执，单独提交成会话日志事件。它只服务历史回放与检视：
                    // 写入失败（或超过上限被放弃）不应让这次模型调用失败，记一条警告并通知实时流放弃即可。
                    match self.store.record_model_attempt_stream(
                        self.session_id,
                        turn_id,
                        &step_id,
                        stream.clone(),
                    ) {
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            self.store.publish_live_assistant_stream(
                                self.session_id,
                                turn_id,
                                &step_id,
                                magi_session_store::LiveAssistantStreamEvent::Abandon,
                            );
                        }
                        Err(error) => {
                            tracing::warn!(
                                session_id = %self.session_id,
                                turn_id,
                                step_id = %step_id,
                                error = %error,
                                "模型尝试流记录写入失败，历史回放将缺少这次尝试的流"
                            );
                            self.store.publish_live_assistant_stream(
                                self.session_id,
                                turn_id,
                                &step_id,
                                magi_session_store::LiveAssistantStreamEvent::Abandon,
                            );
                        }
                    }
                    return Ok(());
                }
                _ => {}
            }
            let mut records = transport_record.lock().expect("transport observation lock");
            let record = records.apply(event)?;
            self.store
                .record_model_step_transport(self.session_id, turn_id, &step_id, record.clone())
                .map_err(journal_error)
        };
        let result = invoke(
            client,
            request,
            &recorded_retry,
            &stopped,
            max_output_tokens,
            &on_transport,
        );
        if let Some(error) = retry_write_error.into_inner().expect("retry journal lock") {
            return Err(step_start_error(error));
        }
        // 非 HTTP 外部执行器：若未走 HTTP 协议块层录制，但执行器确实回传了响应正文或工具调用，
        // 按用户确认（仅记其回传流，不记系统提示和请求头）归一并写入 ModelAttemptStreamRecorded。
        if !stream_recorded.load(Ordering::Acquire)
            && is_external_executor(&capabilities)
            && let Ok(response) = &result
            && let Some(stream) = build_external_executor_attempt_stream(response)
            && let Err(error) =
                self.store
                    .record_model_attempt_stream(self.session_id, turn_id, &step_id, stream)
        {
            tracing::warn!(
                session_id = %self.session_id,
                turn_id,
                step_id = %step_id,
                error = %error,
                "外部执行器回传流记录写入失败"
            );
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
        // 逐次传输回执是用量的权威来源；流式响应的 usage 只出现在这里，不在响应体上。
        let final_transport_usage = transport_record
            .lock()
            .expect("transport observation lock")
            .attempts
            .last()
            .and_then(|record| record.usage.clone());
        if let Some((hooks, identity, _)) = &observed {
            let mut payload = identity.clone();
            if let Some(object) = payload.as_object_mut() {
                object.insert("ok".into(), result.is_ok().into());
                object.insert(
                    "usageTokens".into(),
                    crate::usage_recording::model_usage_tokens(usage).into(),
                );
            }
            hooks.observe("model.response", payload);
        }
        self.log_cache_reuse(
            &step_id,
            previous
                .as_ref()
                .and_then(|step| step.prompt_frame.as_ref()),
            &frame_for_account,
            final_transport_usage.as_ref(),
        );
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
    fn cache_prompt_ir(
        &self,
        request: &ModelInvocationRequest,
    ) -> magi_bridge_client::prompt_ir::PromptIr {
        self.inner.cache_prompt_ir(request)
    }

    fn cache_identity(&self) -> Option<magi_bridge_client::ModelCacheIdentity> {
        self.inner.cache_identity()
    }

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
            None,
            retry,
            cancelled,
            None,
            |client, request, retry, stopped, limit, observer| {
                client.invoke_observed(
                    request,
                    magi_bridge_client::ModelInvocationOptions {
                        on_delta: None,
                        on_retry: retry,
                        is_cancelled: stopped,
                        max_output_tokens: limit,
                    },
                    observer,
                )
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
            None,
            retry,
            cancelled,
            Some(limit),
            |client, request, retry, stopped, limit, observer| {
                client.invoke_observed(
                    request,
                    magi_bridge_client::ModelInvocationOptions {
                        on_delta: None,
                        on_retry: retry,
                        is_cancelled: stopped,
                        max_output_tokens: limit,
                    },
                    observer,
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
    // 递归规范化后再生成身份，消除嵌套 schema 的对象插入顺序差异。
    let value = serde_json::to_value(value).map_err(journal_error)?;
    let value = magi_bridge_client::llm_types::canonicalize_json(&value);
    let bytes = serde_json::to_vec(&value).map_err(journal_error)?;
    Ok(magi_core::hex_lower(Sha256::digest(bytes)))
}

impl From<&PromptFrame> for magi_session_store::ModelPromptFrame {
    fn from(frame: &PromptFrame) -> Self {
        Self {
            ir_version: frame.ir_version,
            stable_prefix_hash: frame.stable_prefix_hash.clone(),
            append_only_history_hash: frame.append_only_history_hash.clone(),
            tool_surface_hash: frame.tool_surface_hash.clone(),
            volatile_tail_hash: frame.volatile_tail_hash.clone(),
            provider_context_hash: frame.provider_context_hash.clone(),
            post_boundary_hash: frame.post_boundary_hash.clone(),
            history_message_count: Some(frame.history_message_count),
            history_chain_hash: Some(frame.history_chain_hash.clone()),
            history_projection: frame.history_projection.clone(),
            prefix_bytes: Some(frame.prefix_bytes),
            tail_bytes: Some(frame.tail_bytes),
            cache_binding: None,
        }
    }
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

fn is_external_executor(capabilities: &magi_bridge_client::ExecutorCapabilities) -> bool {
    capabilities.tools == magi_bridge_client::ExecutorToolMode::ExecutorOwned
        || capabilities.context == magi_bridge_client::ExecutorContextOwnership::ExecutorOwned
}

/// 将外部执行器实际回传的响应（文本、思考、工具调用）经 `AttemptStreamRecorder` 归一为 DSH 紧凑流记录。
/// 仅在执行器未走 HTTP 原始块层录制但回传了有效内容时生成；若无终止原因或无可归一内容则返回 `None`。
fn build_external_executor_attempt_stream(
    response: &ModelResponse,
) -> Option<Vec<serde_json::Value>> {
    let has_reasoning = response
        .thinking
        .as_deref()
        .is_some_and(|text| !text.is_empty());
    let has_content = response
        .content
        .as_deref()
        .is_some_and(|text| !text.is_empty());
    let has_tools = !response.tool_calls.is_empty();
    if !has_reasoning && !has_content && !has_tools {
        return None;
    }
    let now_ms = magi_core::UtcMillis::now().0;
    let mut recorder = magi_bridge_client::AttemptStreamRecorder::default();
    if let Some(thinking) = response.thinking.as_deref().filter(|text| !text.is_empty()) {
        recorder.push(
            now_ms,
            &magi_bridge_client::llm_types::LlmStreamChunk {
                kind: magi_bridge_client::llm_types::LlmStreamChunkType::Thinking,
                content: None,
                tool_call: None,
                thinking: Some(thinking.to_string()),
                usage: None,
                stop_reason: None,
            },
        );
    }
    if let Some(content) = response.content.as_deref().filter(|text| !text.is_empty()) {
        recorder.push(
            now_ms,
            &magi_bridge_client::llm_types::LlmStreamChunk {
                kind: magi_bridge_client::llm_types::LlmStreamChunkType::ContentDelta,
                content: Some(content.to_string()),
                tool_call: None,
                thinking: None,
                usage: None,
                stop_reason: None,
            },
        );
    }
    for (idx, call) in response.tool_calls.iter().enumerate() {
        recorder.push(
            now_ms,
            &magi_bridge_client::llm_types::LlmStreamChunk {
                kind: magi_bridge_client::llm_types::LlmStreamChunkType::ToolCallStart,
                content: None,
                tool_call: Some(magi_bridge_client::llm_types::PartialToolCall {
                    id: Some(call.id.clone()),
                    name: Some(call.function.name.clone()),
                    arguments: Some(serde_json::Value::String(call.function.arguments.clone())),
                    index: Some(idx),
                }),
                thinking: None,
                usage: None,
                stop_reason: None,
            },
        );
    }
    let stop_reason = response.finish_reason.as_deref().or(if has_tools {
        Some("tool_calls")
    } else {
        Some("stop")
    });
    recorder.finish(now_ms, stop_reason)
}

/// 采集一次主线执行步骤的请求快照（系统提示与请求头），外部执行器不记录；
/// 写入前按现有凭据脱敏规则处理，命中时保留脱敏正文并标记 `redacted = true`。
fn build_model_request_snapshot(
    step: &ModelStep,
    request: &ModelInvocationRequest,
    prompt_ir: &magi_bridge_client::prompt_ir::PromptIr,
    capabilities: &magi_bridge_client::ExecutorCapabilities,
    max_output_tokens: Option<u32>,
) -> Option<magi_session_store::ModelRequestSnapshot> {
    if step.purpose != Some(magi_session_store::ModelStepPurpose::Execution)
        || is_external_executor(capabilities)
    {
        return None;
    }
    let binding = step.binding.as_ref()?;
    let model = binding
        .model
        .as_deref()
        .filter(|model| !model.trim().is_empty())?
        .to_string();
    let provider = binding
        .provider
        .as_deref()
        .filter(|provider| !provider.trim().is_empty())
        .unwrap_or(step.provider.as_str())
        .to_string();
    if provider.trim().is_empty() {
        return None;
    }

    let raw_system = request
        .messages
        .iter()
        .flatten()
        .filter(|message| message.role == "system")
        .filter_map(|message| message.content.as_deref())
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let (system_prompt, mut redacted) = magi_core::redact_credentials_text(&raw_system);

    let reasoning_effort = prompt_ir.reasoning_effort.and_then(|effort| {
        serde_json::to_value(effort)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
    });
    let stop = prompt_ir
        .stop
        .iter()
        .map(|entry| {
            let (clean, hit) = magi_core::redact_credentials_text(entry);
            redacted |= hit;
            clean
        })
        .collect();
    let tools = request
        .tools
        .as_deref()
        .filter(|tools| !tools.is_empty())
        .map(|tools| {
            magi_bridge_client::canonical_tool_surface(tools)
                .into_iter()
                .map(|tool| {
                    let (description, hit) =
                        magi_core::redact_credentials_text(&tool.function.description);
                    redacted |= hit;
                    let mut parameters = if tool.function.parameters.is_object() {
                        tool.function.parameters
                    } else {
                        serde_json::json!({"type": "object", "properties": {}})
                    };
                    if magi_core::redact_credentials_json_strings(&mut parameters) {
                        redacted = true;
                    }
                    magi_session_store::ModelRequestToolSchema {
                        name: tool.function.name,
                        description,
                        parameters,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    Some(magi_session_store::ModelRequestSnapshot {
        step_id: step.step_id.clone(),
        system_prompt,
        header: magi_session_store::ModelRequestHeader {
            config: magi_session_store::ModelRequestCallConfig {
                provider,
                model,
                reasoning_effort,
                temperature: prompt_ir.temperature,
                max_tokens: max_output_tokens.or(prompt_ir.max_tokens),
                stop,
            },
            tools,
        },
        context_window: None,
        redacted,
    })
}

#[cfg(test)]
mod tests;
