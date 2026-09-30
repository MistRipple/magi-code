//! GPT Web 的最小运行时边界。
//!
//! Web 页面拥有自己的上下文；本 crate 只负责把本轮 prompt 写入唯一的 Web
//! 槽位并读取回复。工具完全走标准 MCP Gateway，不再有 T2 文本协议或 Web
//! 侧上下文重放。

mod binding;
mod channel;
mod client;
mod driver;
mod errors;
mod factory;
pub mod harness;
mod runtime;
mod site;
mod protocol;

pub use binding::{
    WebConversationBinding, WebConversationMode, WebConversationSyncState, WebSlotClaim,
    WebSlotOwner, WebSlotSnapshot, WebSlotTable, WebTurnLease,
};
pub use channel::{WebModelChannelState, WebModelChannelStatus, status_from_tunnel};
pub use client::{
    BrowserWebModelBridgeClient, WebModelClientConfig, WebModelIdentity,
    normalize_composer_text,
};
pub use driver::{
    DriverFuture, LoginState, SavedConversationSnapshot, SubmitOutcome, TurnState,
    WebMessage, WebModelPageDriver, WriteOutcome,
};
pub use errors::{EngineState, WebModelAction, WebModelError, WebModelErrorCode};
pub use factory::{WebModelClientFactory, WebModelInvocationSpec, new_web_slot_table};
pub use runtime::{WebModelRuntimeEntry, WebModelRuntimeRegistry};
pub use site::{
    CHATGPT_WEB_HOME_URL, CHATGPT_WEB_ORIGIN, CHATGPT_WEB_TEMPORARY_CHAT_URL,
    chatgpt_web_home_url, chatgpt_web_origin, chatgpt_web_temporary_chat_url,
    is_chatgpt_web_engine_id, web_model_engine_id, WEB_MODEL_ENGINE_ID,
};
