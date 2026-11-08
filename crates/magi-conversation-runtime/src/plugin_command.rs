//! 插件用户命令的执行端口。
//!
//! 命令通过原生 Task Turn 接纳、调度、取消与结算，不隐式调用主模型；
//! 这里只定义 Task 执行器向 daemon 请求命令执行的边界，不持有插件状态。

use serde::Serialize;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCommandInvocation {
    pub command_id: String,
    pub turn_id: String,
    /// 与原生工具共用的可信执行投影；不由插件或命令文本提供路径。
    pub context: magi_tool_runtime::ToolExecutionContext,
    pub input: String,
    pub job: Option<magi_app_server_protocol::generated::PluginJobContext>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginCommandResult {
    /// 命令只能提交用户可见文本；Turn、任务和权限事实仍由 daemon 写回。
    pub content: String,
}

pub trait PluginCommandExecutor: Send + Sync {
    /// 同步执行一次命令。`cancelled` 在所属 Turn 被停止时置位，执行方应尽快返回。
    fn invoke(
        &self,
        invocation: PluginCommandInvocation,
        cancelled: &AtomicBool,
    ) -> Result<PluginCommandResult, String>;
}
