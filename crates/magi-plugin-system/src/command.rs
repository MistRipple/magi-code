//! 插件显式命令的唯一后台执行边界。
//!
//! 命令仍然通过普通 Session Turn 接纳和 canonical 终态写回；本模块只负责把
//! 已校验的命令身份交给同一个 QuickJS Worker，并把结构化结果收敛成用户可见文本。

use crate::{APPLICATION_SCOPE, ContributionRef, PluginManager};
use magi_plugin_runtime::{
    CapabilityHandler, Invocation, InvocationIdentity, PluginHost, RunCancellation, RuntimeLimits,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCommandInvocation {
    pub command_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub workspace_id: Option<String>,
    pub input: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginCommandResult {
    /// 命令只能提交用户可见文本；Turn、任务和权限事实仍由 daemon 写回。
    pub content: String,
}

pub trait PluginCommandExecutor: Send + Sync {
    fn invoke(
        &self,
        invocation: PluginCommandInvocation,
        cancellation: &RunCancellation,
    ) -> Result<PluginCommandResult, String>;
}

pub struct PluginCommandRunner {
    manager: Arc<Mutex<PluginManager>>,
    host: PluginHost,
    handler: Arc<dyn CapabilityHandler>,
    limits: RuntimeLimits,
}

impl PluginCommandRunner {
    pub fn new(
        manager: Arc<Mutex<PluginManager>>,
        host: PluginHost,
        handler: Arc<dyn CapabilityHandler>,
        limits: RuntimeLimits,
    ) -> Self {
        Self {
            manager,
            host,
            handler,
            limits,
        }
    }
}

impl PluginCommandExecutor for PluginCommandRunner {
    fn invoke(
        &self,
        invocation: PluginCommandInvocation,
        cancellation: &RunCancellation,
    ) -> Result<PluginCommandResult, String> {
        let ContributionRef {
            plugin_id,
            contribution_id,
        } = ContributionRef::parse("命令", &invocation.command_id)?;
        let scope = APPLICATION_SCOPE;
        let manager = self
            .manager
            .lock()
            .map_err(|_| "插件管理器不可用".to_string())?;
        if !manager
            .is_active_for_scope(&plugin_id, scope)
            .map_err(|error| error.to_string())?
        {
            return Err("插件命令未在当前作用域激活".into());
        }
        let package = manager
            .package(&plugin_id)
            .map_err(|error| error.to_string())?;
        if !package
            .manifest()
            .contributions
            .commands
            .iter()
            .any(|command| command.id == contribution_id)
        {
            return Err("插件命令贡献不存在".into());
        }
        let package_source = package.source().to_string();
        let package_digest = package.digest().to_string();
        drop(manager);
        let lease = crate::PluginRuntimeLease::acquire(
            Arc::clone(&self.manager),
            &plugin_id,
            &package_digest,
            scope,
            cancellation,
        )
        .map_err(|error| error.to_string())?;
        let identity = InvocationIdentity {
            plugin_id,
            package_digest,
            instance_id: scope.to_owned(),
            invocation_id: format!("{}:{}", invocation.turn_id, invocation.command_id),
            workspace_id: invocation.workspace_id.clone(),
            session_id: Some(invocation.session_id.clone()),
            turn_id: Some(invocation.turn_id.clone()),
            run_id: None,
            attempt_id: None,
            contribution: Some(magi_plugin_runtime::InvocationContribution {
                kind: "command".into(),
                id: contribution_id,
            }),
        };
        let input =
            serde_json::to_value(&invocation).map_err(|_| "插件命令输入无效".to_string())?;
        let invocation = Invocation {
            identity,
            source: package_source,
            input,
            limits: self.limits,
        };
        let result = crate::run_invocation_blocking(
            self.host.clone(),
            Arc::clone(&self.handler),
            invocation,
            cancellation.clone(),
            lease,
        )
        .map_err(|error| error.to_string())?;
        let result: PluginCommandResult = serde_json::from_value(result)
            .map_err(|_| "插件命令必须返回 {content:string}".to_string())?;
        if result.content.trim().is_empty() || result.content.len() > 1_048_576 {
            return Err("插件命令返回文本为空或超出上限".into());
        }
        Ok(result)
    }
}
