use crate::{
    APPLICATION_SCOPE, ContributionRef, EngineEvent, PluginManager, PluginSessionEngine,
    SessionEngineAdapter, SessionEngineRequest,
};
use magi_bridge_client::{
    BridgeClientError, BridgeErrorLayer, ModelBridgeClient, ModelInvocationRequest, ModelResponse,
    ModelStreamingDelta,
};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

/// 会话引擎的唯一宿主合同。工作流和任务调度只依赖这个入口，不识别具体站点或
/// 客户端实现；引擎负责消息传递，Turn、权限、用量和终态仍由 daemon 持有。
#[derive(Clone, Debug, PartialEq)]
pub struct SessionEngineInvocationSpec {
    pub session_id: String,
    pub project_id: String,
    pub thread_id: String,
    pub engine_id: String,
    pub binding: Value,
}

pub trait SessionEngineFactory: Send + Sync {
    fn supports(&self, engine_id: &str) -> bool;

    fn build_session_engine(
        &self,
        spec: SessionEngineInvocationSpec,
    ) -> Result<Arc<dyn ModelBridgeClient>, String>;
}

/// 宿主提供的原生插件引擎注册表。
///
/// 原生宿主适配器（例如需要 BrowserAuthority 的站点引擎）仍然使用 Magi 的
/// `SessionEngineFactory` 合同，但由这个注册表按插件引擎命名空间统一路由。
/// 通用 QuickJS 插件工厂会排除这里注册的命名空间，从而保证一个引擎身份只有
/// 一条执行路径；注册表不拥有会话、权限或终态事实。
pub struct NativeSessionEngineFactory {
    entries: Vec<(String, Arc<dyn SessionEngineFactory>)>,
}

impl NativeSessionEngineFactory {
    pub fn new(entries: Vec<(String, Arc<dyn SessionEngineFactory>)>) -> Result<Self, String> {
        if entries.is_empty() {
            return Err("未注册原生插件会话引擎".into());
        }
        let mut namespaces = std::collections::BTreeSet::new();
        let mut normalized = Vec::with_capacity(entries.len());
        for (namespace, factory) in entries {
            let namespace = namespace.trim().trim_end_matches('/').to_string();
            if namespace.is_empty() || !namespaces.insert(namespace.clone()) {
                return Err(format!("原生插件引擎命名空间冲突：{namespace}"));
            }
            normalized.push((namespace, factory));
        }
        Ok(Self {
            entries: normalized,
        })
    }

    /// 返回该注册表占用的命名空间，供通用插件工厂排除相同身份。
    pub fn namespaces(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(namespace, _)| namespace.as_str())
    }
}

impl SessionEngineFactory for NativeSessionEngineFactory {
    fn supports(&self, engine_id: &str) -> bool {
        self.entries.iter().any(|(namespace, factory)| {
            engine_id.starts_with(&format!("{namespace}/")) && factory.supports(engine_id)
        })
    }

    fn build_session_engine(
        &self,
        spec: SessionEngineInvocationSpec,
    ) -> Result<Arc<dyn ModelBridgeClient>, String> {
        let mut matches = self.entries.iter().filter(|(namespace, factory)| {
            spec.engine_id.starts_with(&format!("{namespace}/"))
                && factory.supports(&spec.engine_id)
        });
        let Some((_, factory)) = matches.next() else {
            return Err(format!("未注册原生插件会话引擎：{}", spec.engine_id));
        };
        if matches.next().is_some() {
            return Err(format!("原生插件会话引擎注册冲突：{}", spec.engine_id));
        }
        factory.build_session_engine(spec)
    }
}

/// 会话引擎的唯一路由器。一个引擎身份只能命中一个已注册工厂，不能通过失败后换用
/// 另一实现来掩盖配置或激活错误。
pub struct SessionEngineRouter {
    factories: Vec<Arc<dyn SessionEngineFactory>>,
}

impl SessionEngineRouter {
    pub fn new(factories: Vec<Arc<dyn SessionEngineFactory>>) -> Result<Self, String> {
        if factories.is_empty() {
            return Err("未注册会话引擎工厂".into());
        }
        Ok(Self { factories })
    }
}

impl SessionEngineFactory for SessionEngineRouter {
    fn supports(&self, engine_id: &str) -> bool {
        self.factories
            .iter()
            .any(|factory| factory.supports(engine_id))
    }

    fn build_session_engine(
        &self,
        spec: SessionEngineInvocationSpec,
    ) -> Result<Arc<dyn ModelBridgeClient>, String> {
        let mut matches = self
            .factories
            .iter()
            .filter(|factory| factory.supports(&spec.engine_id));
        let Some(factory) = matches.next() else {
            return Err(format!("未注册会话引擎：{}", spec.engine_id));
        };
        if matches.next().is_some() {
            return Err(format!("会话引擎注册冲突：{}", spec.engine_id));
        }
        factory.build_session_engine(spec)
    }
}

/// 插件贡献的会话引擎工厂。插件包、作用域准入和模型权限均在构造 client 前检查。
pub struct PluginSessionEngineFactory {
    manager: Arc<std::sync::Mutex<PluginManager>>,
    host: magi_plugin_runtime::PluginHost,
    handler: Arc<dyn magi_plugin_runtime::CapabilityHandler>,
    limits: magi_plugin_runtime::RuntimeLimits,
    excluded_namespaces: Vec<String>,
}

impl PluginSessionEngineFactory {
    pub fn new(
        manager: Arc<std::sync::Mutex<PluginManager>>,
        host: magi_plugin_runtime::PluginHost,
        handler: Arc<dyn magi_plugin_runtime::CapabilityHandler>,
        limits: magi_plugin_runtime::RuntimeLimits,
    ) -> Self {
        Self {
            manager,
            host,
            handler,
            limits,
            excluded_namespaces: Vec::new(),
        }
    }

    /// 将由原生插件注册表负责的命名空间从通用包工厂中排除，确保每个引擎身份只有一条路由。
    pub fn with_excluded_namespaces<I, S>(mut self, namespaces: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.excluded_namespaces = namespaces
            .into_iter()
            .map(Into::into)
            .map(|namespace| namespace.trim().trim_end_matches('/').to_string())
            .filter(|namespace| !namespace.is_empty())
            .collect();
        self
    }
}

impl SessionEngineFactory for PluginSessionEngineFactory {
    fn supports(&self, engine_id: &str) -> bool {
        engine_id.starts_with("plugin/")
            && !self
                .excluded_namespaces
                .iter()
                .any(|namespace| engine_id.starts_with(&format!("{namespace}/")))
    }

    fn build_session_engine(
        &self,
        spec: SessionEngineInvocationSpec,
    ) -> Result<Arc<dyn ModelBridgeClient>, String> {
        let ContributionRef {
            plugin_id,
            contribution_id,
        } = ContributionRef::parse("会话引擎", &spec.engine_id)?;
        let scope = APPLICATION_SCOPE;
        let manager = self
            .manager
            .lock()
            .map_err(|_| "插件管理器不可用".to_string())?;
        if !manager
            .is_active_for_scope(&plugin_id, scope)
            .map_err(|error| error.to_string())?
        {
            return Err("插件会话引擎未在当前作用域激活".into());
        }
        if !manager
            .permission_allowed(
                &plugin_id,
                scope,
                magi_app_server_protocol::PluginPermissionKind::Models,
                &contribution_id,
            )
            .map_err(|error| error.to_string())?
        {
            return Err("插件会话引擎未获模型权限".into());
        }
        let package = manager
            .package(&plugin_id)
            .map_err(|error| error.to_string())?;
        if !package
            .manifest()
            .contributions
            .engines
            .iter()
            .any(|engine| engine.id == contribution_id)
        {
            return Err("插件会话引擎贡献不存在".into());
        }
        let identity = magi_plugin_runtime::InvocationIdentity {
            plugin_id,
            package_digest: package.digest().to_string(),
            instance_id: scope.to_owned(),
            // 每次调用的 invocation_id 在 `PluginSessionEngine::invoke` 内重新签发。
            invocation_id: String::new(),
            workspace_id: (!spec.project_id.trim().is_empty()).then_some(spec.project_id.clone()),
            session_id: Some(spec.session_id.clone()),
            turn_id: None,
            run_id: None,
            attempt_id: None,
            contribution: Some(magi_plugin_runtime::InvocationContribution {
                kind: "engine".into(),
                id: contribution_id,
            }),
        };
        let adapter = PluginSessionEngine::new(
            Arc::clone(&self.manager),
            self.host.clone(),
            package.source().to_string(),
            identity,
            Arc::clone(&self.handler),
            self.limits,
            crate::EngineReadiness::Ready,
        );
        Ok(Arc::new(SessionEngineModelClient::new(
            Arc::new(adapter),
            spec.engine_id,
            spec.session_id,
            spec.thread_id,
        )))
    }
}

pub struct SessionEngineModelClient {
    adapter: Arc<dyn SessionEngineAdapter>,
    engine_id: String,
    session_id: String,
    thread_id: String,
}

/// 会话引擎的返回边界必须覆盖适配器资源结算，不能只通知取消后丢下后台线程。
/// 适配器只由可信宿主提供；插件 Worker 的期限、取消和进程回收由 PluginHost 执行。
struct EngineInvocationWorker {
    cancelled: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EngineInvocationWorker {
    fn settle(&mut self) -> Result<(), BridgeClientError> {
        self.cancelled.store(true, Ordering::Release);
        if self
            .thread
            .take()
            .is_some_and(|thread| thread.join().is_err())
        {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Transport,
                code: None,
                message: "会话引擎执行线程异常退出".into(),
            });
        }
        Ok(())
    }
}

impl Drop for EngineInvocationWorker {
    fn drop(&mut self) {
        // 包括取消、错误事件、协议拒绝和调用方回调 panic 的路径。
        let _ = self.settle();
    }
}

impl SessionEngineModelClient {
    pub fn new(
        adapter: Arc<dyn SessionEngineAdapter>,
        engine_id: impl Into<String>,
        session_id: impl Into<String>,
        thread_id: impl Into<String>,
    ) -> Self {
        Self {
            adapter,
            engine_id: engine_id.into(),
            session_id: session_id.into(),
            thread_id: thread_id.into(),
        }
    }

    fn request(&self, request: ModelInvocationRequest) -> SessionEngineRequest {
        // 插件合同只有文本 prompt，完整编码有角色的历史，不能暗中截取最后一条。
        // 不支持的图片和原生工具在进入适配器前拒绝。
        let prompt = match request
            .messages
            .as_ref()
            .filter(|messages| !messages.is_empty())
        {
            Some(messages) => serde_json::json!({
                "prompt": request.prompt,
                "messages": messages,
            })
            .to_string(),
            None => request.prompt,
        };
        SessionEngineRequest {
            engine_id: self.engine_id.clone(),
            session_id: self.session_id.clone(),
            thread_id: self.thread_id.clone(),
            invocation_id: crate::next_invocation_id("engine"),
            prompt,
            images: Vec::new(),
            tool_context: serde_json::json!({ "tools": request.tools }),
        }
    }

    fn error(event: EngineEvent) -> BridgeClientError {
        let detail = match event {
            EngineEvent::Failed { detail, .. } => detail,
            _ => "会话引擎未完成调用".to_string(),
        };
        BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: None,
            message: detail,
        }
    }

    fn invoke_inner(
        &self,
        request: ModelInvocationRequest,
        on_delta: Option<&dyn Fn(&ModelStreamingDelta)>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        if cancelled() {
            return Err(magi_bridge_client::model_invocation_cancelled_error());
        }
        magi_bridge_client::validate_executor_request(&self.executor_capabilities(), &request)?;
        enum WorkerMessage {
            Event(EngineEvent),
            Done(Result<(), BridgeClientError>),
        }
        let cancelled_flag = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled_flag);
        let adapter = Arc::clone(&self.adapter);
        let engine_request = self.request(request);
        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("magi-plugin-engine".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Transport,
                        code: None,
                        message: format!("会话引擎运行时不可用: {error}"),
                    })
                    .and_then(|runtime| {
                        let mut emit = |event| {
                            let _ = sender.send(WorkerMessage::Event(event));
                        };
                        let cancelled = || worker_cancelled.load(Ordering::Acquire);
                        runtime.block_on(async {
                            // readiness 尚未进入 invoke，取消时可直接丢弃检查；不能
                            // 让宿主在 join 中无限等待一个没有取消参数的 readiness。
                            let readiness = adapter.readiness();
                            tokio::pin!(readiness);
                            let readiness = loop {
                                if cancelled() {
                                    return Err(magi_bridge_client::model_invocation_cancelled_error());
                                }
                                tokio::select! {
                                    result = &mut readiness => break result,
                                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
                                }
                            };
                            if cancelled() {
                                return Err(magi_bridge_client::model_invocation_cancelled_error());
                            }
                            if let crate::EngineReadiness::NotReady { code, detail } = readiness {
                                return Err(Self::error(EngineEvent::Failed { code, detail }));
                            }
                            adapter
                                .invoke(engine_request, &mut emit, &cancelled)
                                .await
                                .map_err(Self::error)
                        })
                    });
                let _ = sender.send(WorkerMessage::Done(result));
            })
            .map_err(|error| BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Transport,
                code: None,
                message: format!("无法启动会话引擎执行线程: {error}"),
            })?;
        let mut worker = EngineInvocationWorker {
            cancelled: cancelled_flag,
            thread: Some(thread),
        };

        let mut response = None;
        loop {
            if cancelled() {
                return Err(magi_bridge_client::model_invocation_cancelled_error());
            }
            match receiver.recv_timeout(std::time::Duration::from_millis(10)) {
                Ok(WorkerMessage::Event(_)) if response.is_some() => {
                    return Err(BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Protocol,
                        code: None,
                        message: "会话引擎在完成事件后继续发送事件，拒绝提交重复终态".into(),
                    });
                }
                Ok(WorkerMessage::Event(event)) => match event {
                    EngineEvent::Delta { text } => {
                        if let Some(on_delta) = on_delta {
                            on_delta(&ModelStreamingDelta {
                                content: text,
                                ..ModelStreamingDelta::default()
                            });
                        }
                    }
                    EngineEvent::Completed { content, .. } => {
                        response = Some(ModelResponse::completed(content));
                    }
                    EngineEvent::Failed { .. } => return Err(Self::error(event)),
                },
                Ok(WorkerMessage::Done(Err(error))) => return Err(error),
                Ok(WorkerMessage::Done(Ok(()))) => {
                    worker.settle()?;
                    let response = response.ok_or_else(|| BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Protocol,
                        code: None,
                        message: "会话引擎未返回完成事件".into(),
                    })?;
                    magi_bridge_client::validate_executor_response(
                        &self.executor_capabilities(),
                        &response,
                    )?;
                    return Ok(response);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Transport,
                        code: None,
                        message: "会话引擎执行线程已退出".into(),
                    });
                }
            }
        }
    }
}

impl ModelBridgeClient for SessionEngineModelClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::text_executor_capabilities()
    }

    fn invoke(&self, request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_inner(request, None, &|| false)
    }

    fn invoke_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_inner(request, None, is_cancelled)
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_inner(request, Some(on_delta), &|| false)
    }

    fn invoke_streaming_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        _on_retry: &dyn Fn(&magi_bridge_client::ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_inner(request, Some(on_delta), is_cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RejectingCapabilityHandler;
    impl magi_plugin_runtime::CapabilityHandler for RejectingCapabilityHandler {
        fn call(
            &self,
            _: magi_plugin_runtime::CapabilityRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Value, magi_plugin_runtime::ExecutionError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async { panic!("路由测试不应执行插件能力") })
        }
    }

    #[derive(Default)]
    struct RecordingAdapter(std::sync::Mutex<Vec<SessionEngineRequest>>);

    impl SessionEngineAdapter for RecordingAdapter {
        fn readiness<'a>(
            &'a self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::EngineReadiness> + Send + 'a>>
        {
            Box::pin(async { crate::EngineReadiness::Ready })
        }

        fn invoke<'a>(
            &'a self,
            request: SessionEngineRequest,
            emit: &'a mut dyn FnMut(EngineEvent),
            _cancelled: &'a dyn Fn() -> bool,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EngineEvent>> + 'a>>
        {
            Box::pin(async move {
                self.0.lock().unwrap().push(request);
                emit(EngineEvent::Completed {
                    content: "done".into(),
                    metadata: Value::Null,
                });
                Ok(())
            })
        }
    }

    fn model_request() -> ModelInvocationRequest {
        ModelInvocationRequest {
            provider: "plugin/example/engine".into(),
            prompt: "current input".into(),
            messages: Some(
                [
                    ("developer", "trusted rules"),
                    ("user", "earlier input"),
                    ("assistant", "earlier answer"),
                    ("user", "current input"),
                ]
                .into_iter()
                .map(|(role, content)| magi_bridge_client::ChatMessage {
                    input_request_id: None,
                    context_origin: Default::default(),
                    role: role.into(),
                    content: Some(content.into()),
                    images: vec![],
                    tool_calls: vec![],
                    tool_call_id: None,
                    provider_context: vec![],
                })
                .collect(),
            ),
            tools: None,
            tool_choice: None,
        }
    }

    #[test]
    fn text_engine_preserves_history_and_rejects_unsupported_calls_before_adapter() {
        let adapter = Arc::new(RecordingAdapter::default());
        let client = SessionEngineModelClient::new(
            adapter.clone(),
            "plugin/example/engine",
            "session",
            "thread",
        );
        assert_eq!(
            client.executor_capabilities().tools,
            magi_bridge_client::ExecutorToolMode::None
        );
        client.invoke(model_request()).unwrap();
        let calls = adapter.0.lock().unwrap();
        let prompt: Value = serde_json::from_str(&calls[0].prompt).unwrap();
        assert_eq!(prompt["prompt"], "current input");
        assert_eq!(prompt["messages"].as_array().unwrap().len(), 4);
        assert_eq!(prompt["messages"][0]["content"], "trusted rules");
        assert_eq!(prompt["messages"][1]["content"], "earlier input");
        drop(calls);
        let mut request = model_request();
        request.tool_choice = Some(magi_bridge_client::ChatToolChoice::force_function(
            "shell_exec",
        ));
        assert!(
            client
                .invoke(request)
                .unwrap_err()
                .to_string()
                .contains("executor_native_tools_unsupported")
        );
        let mut request = model_request();
        request.messages.as_mut().unwrap()[1].images.push(
            magi_bridge_client::llm_types::ImageSource {
                kind: "base64".into(),
                media_type: "image/png".into(),
                data: "data".into(),
            },
        );
        assert!(
            client
                .invoke(request)
                .unwrap_err()
                .to_string()
                .contains("executor_image_unsupported")
        );
        assert!(
            client
                .invoke_with_cancellation(model_request(), &|| true)
                .is_err()
        );
        assert!(
            client
                .invoke_with_cancellation_and_output_limit(model_request(), 100, &|| false)
                .unwrap_err()
                .to_string()
                .contains("executor_output_limit_unsupported")
        );
        assert_eq!(adapter.0.lock().unwrap().len(), 1);
    }

    struct NamespaceFactory(&'static str);

    struct EventAdapter(Vec<EngineEvent>);
    impl SessionEngineAdapter for EventAdapter {
        fn readiness<'a>(
            &'a self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::EngineReadiness> + Send + 'a>>
        {
            Box::pin(async { crate::EngineReadiness::Ready })
        }
        fn invoke<'a>(
            &'a self,
            _: SessionEngineRequest,
            emit: &'a mut dyn FnMut(EngineEvent),
            _: &'a dyn Fn() -> bool,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EngineEvent>> + 'a>>
        {
            Box::pin(async move {
                for event in &self.0 {
                    emit(event.clone());
                }
                Ok(())
            })
        }
    }

    #[test]
    fn engine_deltas_remain_increments_and_terminal_event_is_unique() {
        let completed = EngineEvent::Completed {
            content: "ab".into(),
            metadata: Value::Null,
        };
        let client = SessionEngineModelClient::new(
            Arc::new(EventAdapter(vec![
                EngineEvent::Delta { text: "a".into() },
                EngineEvent::Delta { text: "b".into() },
                completed.clone(),
            ])),
            "plugin/example/engine",
            "session",
            "thread",
        );
        let deltas = std::cell::RefCell::new(Vec::new());
        let response = client
            .invoke_streaming(model_request(), &|delta| {
                deltas.borrow_mut().push(delta.content.clone())
            })
            .unwrap();
        assert_eq!(response.content.as_deref(), Some("ab"));
        assert_eq!(deltas.into_inner(), vec!["a", "b"]);
        for after in [
            completed.clone(),
            EngineEvent::Delta {
                text: "late".into(),
            },
        ] {
            let client = SessionEngineModelClient::new(
                Arc::new(EventAdapter(vec![completed.clone(), after])),
                "plugin/example/engine",
                "session",
                "thread",
            );
            assert!(
                client
                    .invoke(model_request())
                    .unwrap_err()
                    .to_string()
                    .contains("完成事件后")
            );
        }
        let client = SessionEngineModelClient::new(
            Arc::new(EventAdapter(vec![EngineEvent::Completed {
                content: " \n".into(),
                metadata: Value::Null,
            }])),
            "plugin/example/engine",
            "session",
            "thread",
        );
        for result in [
            client.invoke(model_request()),
            client.invoke_streaming(model_request(), &|_| {}),
        ] {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("executor_incomplete_response")
            );
        }
    }

    struct SettlingAdapter {
        started: Arc<AtomicBool>,
        settled: Arc<AtomicBool>,
        events: Vec<EngineEvent>,
    }

    impl SessionEngineAdapter for SettlingAdapter {
        fn readiness<'a>(
            &'a self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::EngineReadiness> + Send + 'a>>
        {
            Box::pin(async { crate::EngineReadiness::Ready })
        }

        fn invoke<'a>(
            &'a self,
            _: SessionEngineRequest,
            emit: &'a mut dyn FnMut(EngineEvent),
            cancelled: &'a dyn Fn() -> bool,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EngineEvent>> + 'a>>
        {
            Box::pin(async move {
                self.started.store(true, Ordering::Release);
                for event in &self.events {
                    emit(event.clone());
                }
                while !cancelled() {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
                // 模拟 Worker 收到取消后结算资源，取消通知不等于已退出。
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                self.settled.store(true, Ordering::Release);
                Ok(())
            })
        }
    }

    #[test]
    fn engine_failure_protocol_rejection_and_cancel_wait_for_adapter_settlement() {
        for events in [
            vec![EngineEvent::Failed {
                code: "provider_failure".into(),
                detail: "failed".into(),
            }],
            vec![
                EngineEvent::Completed {
                    content: "done".into(),
                    metadata: Value::Null,
                },
                EngineEvent::Delta {
                    text: "late".into(),
                },
            ],
            vec![],
        ] {
            let started = Arc::new(AtomicBool::new(false));
            let settled = Arc::new(AtomicBool::new(false));
            let external_cancel = events.is_empty();
            let client = SessionEngineModelClient::new(
                Arc::new(SettlingAdapter {
                    started: started.clone(),
                    settled: settled.clone(),
                    events,
                }),
                "plugin/example/engine",
                "session",
                "thread",
            );
            let result = client.invoke_with_cancellation(model_request(), &|| {
                external_cancel && started.load(Ordering::Acquire)
            });
            assert!(result.is_err());
            assert!(
                settled.load(Ordering::Acquire),
                "返回错误前必须等待原适配器真正退出"
            );
        }
    }

    #[test]
    fn streaming_callback_panic_still_settles_the_adapter() {
        let settled = Arc::new(AtomicBool::new(false));
        let client = SessionEngineModelClient::new(
            Arc::new(SettlingAdapter {
                started: Arc::new(AtomicBool::new(false)),
                settled: settled.clone(),
                events: vec![EngineEvent::Delta { text: "a".into() }],
            }),
            "plugin/example/engine",
            "session",
            "thread",
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            client.invoke_streaming(model_request(), &|_| panic!("consumer panicked"))
        }));
        assert!(result.is_err());
        assert!(settled.load(Ordering::Acquire));
    }

    struct PendingReadinessAdapter {
        started: Arc<AtomicBool>,
        settled: Arc<AtomicBool>,
    }

    impl SessionEngineAdapter for PendingReadinessAdapter {
        fn readiness<'a>(
            &'a self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::EngineReadiness> + Send + 'a>>
        {
            Box::pin(async {
                struct CheckGuard<'a>(&'a AtomicBool);
                impl Drop for CheckGuard<'_> {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Release);
                    }
                }
                let _check = CheckGuard(&self.settled);
                self.started.store(true, Ordering::Release);
                std::future::pending().await
            })
        }

        fn invoke<'a>(
            &'a self,
            _: SessionEngineRequest,
            _: &'a mut dyn FnMut(EngineEvent),
            _: &'a dyn Fn() -> bool,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EngineEvent>> + 'a>>
        {
            panic!("取消可用性检查后不能启动调用")
        }
    }

    #[test]
    fn cancellation_during_readiness_drops_check_and_joins_worker() {
        let started = Arc::new(AtomicBool::new(false));
        let settled = Arc::new(AtomicBool::new(false));
        let client = SessionEngineModelClient::new(
            Arc::new(PendingReadinessAdapter {
                started: started.clone(),
                settled: settled.clone(),
            }),
            "plugin/example/engine",
            "session",
            "thread",
        );
        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let result = client
                .invoke_with_cancellation(model_request(), &|| started.load(Ordering::Acquire));
            sender.send(result).unwrap();
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("取消不能在 readiness 的线程结算中死锁");
        assert!(result.is_err());
        assert!(settled.load(Ordering::Acquire));
        thread.join().unwrap();
    }

    #[test]
    fn cached_plugin_engine_cannot_start_after_drain_or_disable() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = PluginManager::open(root.path()).unwrap();
        let manifest = serde_json::from_value(serde_json::json!({
            "sdkVersion":1,"id":"test.engine","version":"1.0.0","name":"Engine","description":"",
            "backend":"plugin.mjs","dataSchemaVersion":1,
            "settingsSchema":{"type":"object","additionalProperties":false},
            "permissions":[{"kind":"models","scope":"application","targets":["default"]}],
            "contributions":{"engines":[{"id":"default","title":"Engine","description":""}]}
        }))
        .unwrap();
        let package = crate::PluginPackage::from_manifest_and_source(
            manifest,
            "export default () => ({events: []})",
        )
        .unwrap();
        manager
            .install(
                &package,
                crate::PluginSource::Local {
                    name: "engine.zip".into(),
                },
            )
            .unwrap();
        manager.enable("test.engine", APPLICATION_SCOPE).unwrap();
        manager
            .authorize(
                "test.engine",
                APPLICATION_SCOPE,
                package.manifest().permissions.clone(),
            )
            .unwrap();
        manager.activate("test.engine", APPLICATION_SCOPE).unwrap();
        let manager = Arc::new(std::sync::Mutex::new(manager));
        let factory = PluginSessionEngineFactory::new(
            manager.clone(),
            magi_plugin_runtime::PluginHost::new(
                // 拒绝必须发生在 Worker 启动之前，不能退化为启动失败。
                root.path().join("missing-worker"),
                magi_process::ManagedProcessGroup::new(),
            ),
            Arc::new(RejectingCapabilityHandler),
            magi_plugin_runtime::RuntimeLimits::default(),
        );
        let client = factory
            .build_session_engine(SessionEngineInvocationSpec {
                engine_id: "plugin/test.engine/default".into(),
                session_id: "session".into(),
                project_id: String::new(),
                thread_id: "thread".into(),
                binding: Value::Null,
            })
            .unwrap();
        let drain =
            PluginManager::begin_deactivation(manager.clone(), "test.engine", APPLICATION_SCOPE)
                .unwrap();
        assert!(
            client
                .invoke(model_request())
                .unwrap_err()
                .to_string()
                .contains("排空")
        );
        assert_eq!(manager.lock().unwrap().plugin_lease_count("test.engine"), 0);
        {
            let mut manager = manager.lock().unwrap();
            manager
                .deactivate("test.engine", APPLICATION_SCOPE)
                .unwrap();
            drain.commit(&mut manager);
            manager.disable("test.engine", APPLICATION_SCOPE).unwrap();
        }
        assert!(
            client
                .invoke(model_request())
                .unwrap_err()
                .to_string()
                .contains("未激活")
        );
        assert_eq!(manager.lock().unwrap().plugin_lease_count("test.engine"), 0);
    }

    #[test]
    fn native_plugin_namespace_keeps_engine_routing_single_owner() {
        let root = tempfile::tempdir().expect("create plugin manager root");
        let manager = PluginManager::open(root.path()).expect("open empty plugin manager");
        let factory = PluginSessionEngineFactory::new(
            Arc::new(std::sync::Mutex::new(manager)),
            magi_plugin_runtime::PluginHost::new(
                std::path::PathBuf::from("/tmp/magi-plugin-worker"),
                magi_process::ManagedProcessGroup::default(),
            ),
            Arc::new(RejectingCapabilityHandler),
            magi_plugin_runtime::RuntimeLimits::default(),
        )
        .with_excluded_namespaces(["plugin/openai.chatgpt-web"]);
        assert!(factory.supports("plugin/example/engine"));
        assert!(!factory.supports("plugin/openai.chatgpt-web/default"));

        let native = NativeSessionEngineFactory::new(vec![(
            "plugin/openai.chatgpt-web".into(),
            Arc::new(NamespaceFactory("plugin/openai.chatgpt-web")),
        )])
        .expect("native engine registry");
        let router = SessionEngineRouter::new(vec![Arc::new(factory), Arc::new(native)])
            .expect("router should register host and plugin factories");
        assert!(router.supports("plugin/openai.chatgpt-web/default"));
        assert!(router.supports("plugin/example/engine"));
    }

    impl SessionEngineFactory for NamespaceFactory {
        fn supports(&self, engine_id: &str) -> bool {
            engine_id.starts_with(self.0)
        }

        fn build_session_engine(
            &self,
            _spec: SessionEngineInvocationSpec,
        ) -> Result<Arc<dyn ModelBridgeClient>, String> {
            Err("测试工厂不应被调用".into())
        }
    }

    #[test]
    fn router_rejects_unregistered_and_ambiguous_engine_ids() {
        let router = SessionEngineRouter::new(vec![Arc::new(NamespaceFactory("plugin/"))])
            .expect("router should have one factory");
        assert!(router.supports("plugin/example/engine"));
        assert!(!router.supports("plugin:example:engine"));
        assert!(!router.supports("builtin/default"));
        let error = match router.build_session_engine(SessionEngineInvocationSpec {
            session_id: String::new(),
            project_id: String::new(),
            thread_id: String::new(),
            engine_id: "builtin/default".into(),
            binding: Value::Null,
        }) {
            Ok(_) => panic!("unregistered engine must fail closed"),
            Err(error) => error,
        };
        assert!(error.contains("未注册会话引擎"));

        let ambiguous = SessionEngineRouter::new(vec![
            Arc::new(NamespaceFactory("plugin/")),
            Arc::new(NamespaceFactory("plugin/")),
        ])
        .unwrap();
        let error = match ambiguous.build_session_engine(SessionEngineInvocationSpec {
            session_id: String::new(),
            project_id: String::new(),
            thread_id: String::new(),
            engine_id: "plugin/example/engine".into(),
            binding: Value::Null,
        }) {
            Ok(_) => panic!("ambiguous engine must fail closed"),
            Err(error) => error,
        };
        assert!(error.contains("注册冲突"));
    }
}
