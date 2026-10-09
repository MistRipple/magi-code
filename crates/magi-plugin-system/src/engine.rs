//! 可替换会话引擎的宿主合同。
//!
//! 引擎只负责消息传递与流式结果；Turn、权限、上下文预算、取消和持久化仍由 daemon
//! 持有。适配器不能提交工具成功、审批结果或终态事实。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::{future::Future, pin::Pin};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum EngineReadiness {
    Ready,
    NotReady { code: String, detail: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEngineRequest {
    pub engine_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub attempt_id: String,
    pub prompt: String,
    pub images: Vec<Value>,
    pub tool_context: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum EngineEvent {
    Delta { text: String },
    Completed { content: String, metadata: Value },
    Failed { code: String, detail: String },
}

pub trait SessionEngineAdapter: Send + Sync {
    fn readiness<'a>(&'a self) -> Pin<Box<dyn Future<Output = EngineReadiness> + Send + 'a>>;
    fn invoke<'a>(
        &'a self,
        request: SessionEngineRequest,
        emit: &'a mut (dyn FnMut(EngineEvent) + Send),
        cancelled: &'a (dyn Fn() -> bool + Send + Sync),
    ) -> Pin<Box<dyn Future<Output = Result<(), EngineEvent>> + Send + 'a>>;
}

/// 由唯一 QuickJS Worker 承载的插件会话引擎适配器。
/// 后台返回 `{events:[...]}`，宿主逐个校验并发布事件；插件不能直接写 Turn。
pub struct PluginSessionEngine {
    host: magi_plugin_runtime::PluginHost,
    source: String,
    identity: magi_plugin_runtime::InvocationIdentity,
    handler: Arc<dyn magi_plugin_runtime::CapabilityHandler>,
    limits: magi_plugin_runtime::RuntimeLimits,
    readiness: EngineReadiness,
}

impl PluginSessionEngine {
    pub fn new(
        host: magi_plugin_runtime::PluginHost,
        source: impl Into<String>,
        identity: magi_plugin_runtime::InvocationIdentity,
        handler: Arc<dyn magi_plugin_runtime::CapabilityHandler>,
        limits: magi_plugin_runtime::RuntimeLimits,
        readiness: EngineReadiness,
    ) -> Self {
        Self {
            host,
            source: source.into(),
            identity,
            handler,
            limits,
            readiness,
        }
    }
}

impl SessionEngineAdapter for PluginSessionEngine {
    fn readiness<'a>(&'a self) -> Pin<Box<dyn Future<Output = EngineReadiness> + Send + 'a>> {
        Box::pin(async move { self.readiness.clone() })
    }

    fn invoke<'a>(
        &'a self,
        request: SessionEngineRequest,
        emit: &'a mut (dyn FnMut(EngineEvent) + Send),
        cancelled: &'a (dyn Fn() -> bool + Send + Sync),
    ) -> Pin<Box<dyn Future<Output = Result<(), EngineEvent>> + Send + 'a>> {
        Box::pin(async move {
            if cancelled() {
                return Err(EngineEvent::Failed {
                    code: "cancelled".into(),
                    detail: "会话引擎调用已取消".into(),
                });
            }
            let mut identity = self.identity.clone();
            identity.invocation_id = format!("{}:{}", request.turn_id, request.attempt_id);
            let invocation = magi_plugin_runtime::Invocation {
                identity,
                source: self.source.clone(),
                input: serde_json::to_value(&request).map_err(|_| EngineEvent::Failed {
                    code: "invalid_request".into(),
                    detail: "会话引擎输入无效".into(),
                })?,
                limits: self.limits,
            };
            let cancellation = magi_plugin_runtime::RunCancellation::default();
            let future = self
                .host
                .invoke(invocation, self.handler.as_ref(), &cancellation);
            tokio::pin!(future);
            let result = loop {
                tokio::select! {
                    biased;
                    result = &mut future => break result,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                        if cancelled() { cancellation.cancel(); }
                    }
                }
            }
            .map_err(|error| EngineEvent::Failed {
                code: format!("{error:?}"),
                detail: "会话引擎执行失败".into(),
            })?;
            let events = result
                .get("events")
                .and_then(Value::as_array)
                .ok_or_else(|| EngineEvent::Failed {
                    code: "invalid_response".into(),
                    detail: "会话引擎未返回 events 数组".into(),
                })?;
            for value in events {
                let event: EngineEvent =
                    serde_json::from_value(value.clone()).map_err(|_| EngineEvent::Failed {
                        code: "invalid_response".into(),
                        detail: "会话引擎事件无效".into(),
                    })?;
                emit(event);
            }
            Ok(())
        })
    }
}
