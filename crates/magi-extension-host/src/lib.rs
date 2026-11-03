//! 受信 Node 扩展宿主：协议传输、宿主进程监管与 ExtensionBus。
//!
//! 规范见 `docs/extension-protocol-v1.md`。daemon 通过该宿主承载钩子、工具、命令和公开服务，
//! 会话、审批与持久化事实仍由 Magi 原生服务拥有。

pub mod breaker;

/// 允许提案改写已装配历史的钩子；缓存归因与保护规则见 `docs/prompt-cache-lineage-architecture.md`。
pub const EXTENSION_REWRITE_HOOKS: [&str; 2] = ["context.assemble", "compaction.before"];
pub mod bus;
pub mod frame;
pub mod instance;
pub mod peer;
pub mod protocol;
pub mod supervisor;
pub mod transport;

use crate::peer::BoxFuture;
use crate::protocol::RpcError;
use serde_json::Value;

/// daemon 暴露给受信扩展的业务服务端口。实现必须在 Magi 原生事实源上执行准入，
/// 不得让 Node 直接写 canonical 日志或复制会话/任务状态。
pub trait HostService: Send + Sync + 'static {
    /// 每次宿主启动（含崩溃后重启）从原生设置事实读取配置。
    fn configuration(&self, _plugin_id: String) -> BoxFuture<Result<Option<Value>, RpcError>> {
        Box::pin(async { Ok(None) })
    }
    fn request_scoped(
        &self,
        plugin_id: String,
        method: String,
        params: Value,
        _cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> BoxFuture<Result<Value, RpcError>> {
        self.request_with_identity(plugin_id, method, params)
    }
    fn request_with_identity(
        &self,
        plugin_id: String,
        method: String,
        params: Value,
    ) -> BoxFuture<Result<Value, RpcError>>;
    /// 宿主实例已退出（崩溃、停止或被替换），进程树已回收。实现应撤销该插件在本次运行中
    /// 登记的运行期事实（例如贡献的模型数据源）；下一个实例重新激活时自行提交。
    fn instance_detached(&self, _plugin_id: &str) {}
}

/// Node 侧宿主运行时（`runtime/host.mjs`）。daemon 在状态目录里按内容摘要落盘后交给 Node 执行。
pub const HOST_RUNTIME_SOURCE: &str = include_str!("../runtime/host.mjs");
