//! 共享 Cordis Node 宿主的进程边界和内部控制传输。

/// 允许提案改写已装配历史的钩子名清单；提交改写时须重新绑定缓存谱系。
pub const EXTENSION_REWRITE_HOOKS: [&str; 2] = ["context.assemble", "compaction.before"];
pub mod frame;
pub mod peer;
pub mod protocol;
pub mod shared;
pub mod transport;

use crate::protocol::RpcError;
use serde_json::Value;

/// 当前调用的临时进度；持久结果仍由业务服务提交。发送失败必须停止交付。
pub type ServiceProgress = std::sync::Arc<dyn Fn(Value) -> Result<(), RpcError> + Send + Sync>;
