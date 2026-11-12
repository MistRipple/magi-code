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
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        cancelled: &dyn Fn() -> bool,
        max_output_tokens: Option<u32>,
        invoke: impl FnOnce(
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
        let mut prompt_frame = PromptFrame::from_ir(&client.cache_prompt_ir(&request));
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
        self.store
            .begin_model_step(self.session_id, turn_id, step)
            .map_err(step_start_error)?;
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
        let on_transport = |event: &magi_bridge_client::ModelTransportEvent| {
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

#[cfg(test)]
mod tests;
