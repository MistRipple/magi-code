use crate::{
    EngineEvent, PluginManager, PluginSessionEngine, SessionEngineAdapter, SessionEngineRequest,
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
/// 客户端实现；引擎负责消息传递，Turn、权限、预算和终态仍由 daemon 持有。
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

    fn identity(engine_id: &str) -> Result<(String, String), String> {
        let rest = engine_id
            .strip_prefix("plugin/")
            .ok_or_else(|| "插件会话引擎身份无效".to_string())?;
        let (plugin_id, contribution_id) = rest
            .rsplit_once('/')
            .filter(|(plugin, contribution)| !plugin.is_empty() && !contribution.is_empty())
            .ok_or_else(|| "插件会话引擎身份必须为 plugin/{插件}/{贡献}".to_string())?;
        Ok((plugin_id.to_string(), contribution_id.to_string()))
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
        let (plugin_id, contribution_id) = Self::identity(&spec.engine_id)?;
        let scope = if spec.project_id.trim().is_empty() {
            "application".to_string()
        } else {
            format!("workspace:{}", spec.project_id)
        };
        let manager = self
            .manager
            .lock()
            .map_err(|_| "插件管理器不可用".to_string())?;
        if !manager
            .is_active_for_scope(&plugin_id, &scope)
            .map_err(|error| error.to_string())?
        {
            return Err("插件会话引擎未在当前作用域激活".into());
        }
        if !manager
            .permission_allowed(
                &plugin_id,
                &scope,
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
        let contribution = package
            .manifest()
            .contributions
            .engines
            .iter()
            .find(|engine| engine.id == contribution_id)
            .ok_or_else(|| "插件会话引擎贡献不存在".to_string())?;
        let _ = contribution;
        let identity = magi_plugin_runtime::InvocationIdentity {
            plugin_id: plugin_id.clone(),
            package_digest: package.digest().to_string(),
            instance_id: scope.clone(),
            invocation_id: format!("{}:{}", spec.session_id, spec.thread_id),
            workspace_id: (!spec.project_id.trim().is_empty()).then_some(spec.project_id.clone()),
            run_id: None,
            attempt_id: None,
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
        )))
    }
}

pub struct SessionEngineModelClient {
    adapter: Arc<dyn SessionEngineAdapter>,
    engine_id: String,
    session_id: String,
    turn_id: String,
    attempt_id: String,
}

impl SessionEngineModelClient {
    pub fn new(
        adapter: Arc<dyn SessionEngineAdapter>,
        engine_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            adapter,
            engine_id: engine_id.into(),
            session_id: session_id.into(),
            turn_id: String::new(),
            attempt_id: String::new(),
        }
    }

    fn request(&self, request: ModelInvocationRequest) -> SessionEngineRequest {
        let prompt = request
            .messages
            .as_ref()
            .and_then(|messages| {
                messages
                    .iter()
                    .rev()
                    .find_map(|message| message.content.clone())
            })
            .unwrap_or(request.prompt);
        SessionEngineRequest {
            engine_id: self.engine_id.clone(),
            session_id: self.session_id.clone(),
            turn_id: self.turn_id.clone(),
            attempt_id: self.attempt_id.clone(),
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
        enum WorkerMessage {
            Event(EngineEvent),
            Done(Result<(), BridgeClientError>),
        }
        let cancelled_flag = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled_flag);
        let adapter = Arc::clone(&self.adapter);
        let engine_request = self.request(request);
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
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
                    runtime
                        .block_on(adapter.invoke(engine_request, &mut emit, &cancelled))
                        .map_err(|event| Self::error(event))
                });
            let _ = sender.send(WorkerMessage::Done(result));
        });

        let mut response = None;
        loop {
            if cancelled() {
                cancelled_flag.store(true, Ordering::Release);
                return Err(BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Transport,
                    code: Some(-32800),
                    message: "会话引擎调用已取消".into(),
                });
            }
            match receiver.recv_timeout(std::time::Duration::from_millis(10)) {
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
                    return response.ok_or_else(|| BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Protocol,
                        code: None,
                        message: "会话引擎未返回完成事件".into(),
                    });
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

    struct NamespaceFactory(&'static str);

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

    struct RejectingCapabilityHandler;

    impl magi_plugin_runtime::CapabilityHandler for RejectingCapabilityHandler {
        fn call<'a>(
            &'a self,
            _request: magi_plugin_runtime::CapabilityRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Value, magi_plugin_runtime::ExecutionError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(std::future::ready(Err(
                magi_plugin_runtime::ExecutionError::new(
                    magi_plugin_runtime::ExecutionErrorCode::InvalidRequest,
                    "测试能力处理器不应被调用",
                ),
            )))
        }
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
