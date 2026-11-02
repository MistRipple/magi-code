//! 受信 Node 扩展宿主：协议传输、宿主进程监管与 ExtensionBus。
//!
//! 规范见 `docs/extension-protocol-v1.md`。本 crate 目前是独立可测的宿主内核，
//! 尚未接入 conversation-runtime 的实际调用链。

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

/// Node 侧宿主运行时（`runtime/host.mjs`）。daemon 在状态目录里按内容摘要落盘后交给 Node 执行。
pub const HOST_RUNTIME_SOURCE: &str = include_str!("../runtime/host.mjs");
