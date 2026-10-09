use crate::workflow::{WorkflowCore, WorkflowDecision, WorkflowError, WorkflowInput};
use magi_plugin_runtime::{
    CapabilityHandler, Invocation, InvocationIdentity, PluginHost, RunCancellation, RuntimeLimits,
};
use std::sync::Arc;

/// 将已固定版本的插件后台入口适配为工作流核心。每次决策都启动一次受管 Worker，
/// 不保留插件全局状态，也不在插件失败时隐式换用默认核心。
pub struct PluginWorkflowCore {
    host: PluginHost,
    source: String,
    identity: InvocationIdentity,
    limits: RuntimeLimits,
    handler: Arc<dyn CapabilityHandler>,
}

impl PluginWorkflowCore {
    pub fn new(
        host: PluginHost,
        source: impl Into<String>,
        identity: InvocationIdentity,
        handler: Arc<dyn CapabilityHandler>,
        limits: RuntimeLimits,
    ) -> Self {
        Self {
            host,
            source: source.into(),
            identity,
            limits,
            handler,
        }
    }
}

impl WorkflowCore for PluginWorkflowCore {
    fn decide<'a>(
        &'a self,
        input: WorkflowInput,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<WorkflowDecision, WorkflowError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let run_id = input.run_id.clone();
            let invocation = Invocation {
                identity: self.identity.clone(),
                source: self.source.clone(),
                input: serde_json::to_value(&input).map_err(|_| WorkflowError {
                    message: "工作流输入无法序列化".into(),
                })?,
                limits: self.limits,
            };
            let result = self
                .host
                .invoke(
                    invocation,
                    self.handler.as_ref(),
                    &RunCancellation::default(),
                )
                .await
                .map_err(|error| WorkflowError {
                    message: format!("插件工作流执行失败: {error}"),
                })?;
            let decision: WorkflowDecision =
                serde_json::from_value(result).map_err(|_| WorkflowError {
                    message: "插件工作流返回值不符合动作合同".into(),
                })?;
            decision
                .validate(&run_id, &input)
                .map_err(|error| WorkflowError {
                    message: error.message,
                })?;
            Ok(decision)
        })
    }
}
