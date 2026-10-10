//! 插件显式命令的唯一后台执行边界。
//!
//! 命令仍然通过普通 Session Turn 接纳和 canonical 终态写回；本模块只负责把
//! 已校验的命令身份交给同一个 QuickJS Worker，并把结构化结果收敛成用户可见文本。

use crate::PluginManager;
use magi_plugin_runtime::{
    CapabilityHandler, Invocation, InvocationIdentity, PluginHost, RunCancellation, RuntimeLimits,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

    fn identity(command_id: &str) -> Result<(String, String), String> {
        let rest = command_id
            .strip_prefix("plugin/")
            .ok_or_else(|| "插件命令身份无效".to_string())?;
        let (plugin_id, contribution_id) = rest
            .rsplit_once('/')
            .filter(|(plugin, contribution)| !plugin.is_empty() && !contribution.is_empty())
            .ok_or_else(|| "插件命令身份必须为 plugin/{插件}/{贡献}".to_string())?;
        Ok((plugin_id.to_string(), contribution_id.to_string()))
    }

    fn invoke_worker(
        &self,
        package_source: String,
        identity: InvocationIdentity,
        input: Value,
        cancellation: &RunCancellation,
    ) -> Result<Value, String> {
        let invocation = Invocation {
            identity,
            source: package_source,
            input,
            limits: self.limits,
        };
        let host = self.host.clone();
        let handler = Arc::clone(&self.handler);
        let cancellation = cancellation.clone();
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "插件命令执行器不可用".to_string())
                .and_then(|runtime| {
                    runtime
                        .block_on(host.invoke(invocation, handler.as_ref(), &cancellation))
                        .map_err(|error| error.to_string())
                })
        })
        .join()
        .map_err(|_| "插件命令执行线程异常退出".to_string())?
    }
}

impl PluginCommandExecutor for PluginCommandRunner {
    fn invoke(
        &self,
        invocation: PluginCommandInvocation,
        cancellation: &RunCancellation,
    ) -> Result<PluginCommandResult, String> {
        let (plugin_id, contribution_id) = Self::identity(&invocation.command_id)?;
        let scope = invocation
            .workspace_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .map(|id| format!("workspace:{id}"))
            .unwrap_or_else(|| "application".to_string());
        let manager = self
            .manager
            .lock()
            .map_err(|_| "插件管理器不可用".to_string())?;
        if !manager
            .is_active_for_scope(&plugin_id, &scope)
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
        let identity = InvocationIdentity {
            plugin_id,
            package_digest: package.digest().to_string(),
            instance_id: scope,
            invocation_id: format!("{}:{}", invocation.turn_id, invocation.command_id),
            workspace_id: invocation.workspace_id.clone(),
            run_id: Some(invocation.turn_id.clone()),
            attempt_id: Some(invocation.session_id.clone()),
        };
        drop(manager);
        let input =
            serde_json::to_value(&invocation).map_err(|_| "插件命令输入无效".to_string())?;
        let result = self.invoke_worker(package_source, identity, input, cancellation)?;
        let result: PluginCommandResult = serde_json::from_value(result)
            .map_err(|_| "插件命令必须返回 {content:string}".to_string())?;
        if result.content.trim().is_empty() || result.content.len() > 1_048_576 {
            return Err("插件命令返回文本为空或超出上限".into());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::PluginCommandRunner;

    #[test]
    fn command_identity_is_strictly_namespaced() {
        assert_eq!(
            PluginCommandRunner::identity("plugin/acme.tools/open"),
            Ok(("acme.tools".into(), "open".into()))
        );
        assert!(PluginCommandRunner::identity("acme.tools/open").is_err());
        assert!(PluginCommandRunner::identity("plugin//open").is_err());
        assert!(PluginCommandRunner::identity("plugin/acme.tools/").is_err());
    }
}
