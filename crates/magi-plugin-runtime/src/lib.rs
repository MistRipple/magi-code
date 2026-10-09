//! daemon 所有的插件执行边界。这里只执行隔离 JavaScript，不拥有业务权限或任务事实。
//!
//! 宿主传入权威身份与受控能力处理器；插件经同步 SDK 桥请求能力。宿主的超时和取消
//! 覆盖 JavaScript 以及等待中的能力请求，不向插件暴露系统环境、文件或网络 API。

mod host;
mod protocol;
mod worker;

pub use host::{CapabilityHandler, CapabilityRequest, PluginHost, RunCancellation};
pub use protocol::{
    ExecutionError, ExecutionErrorCode, Invocation, InvocationIdentity, RuntimeLimits,
};
pub use worker::run_worker_stdio;
