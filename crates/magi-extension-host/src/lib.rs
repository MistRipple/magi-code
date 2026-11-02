//! 受信 Node 扩展宿主：协议传输、宿主进程监管与 ExtensionBus。
//!
//! 规范见 `docs/extension-protocol-v1.md`。本 crate 目前是独立可测的宿主内核，
//! 尚未接入 conversation-runtime 的实际调用链。

pub mod breaker;
pub mod bus;
pub mod frame;
pub mod instance;
pub mod peer;
pub mod protocol;
pub mod supervisor;
pub mod transport;
