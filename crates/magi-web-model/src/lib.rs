//! GPT Web 的最小运行时边界。
//!
//! Web 页面拥有自己的上下文；本 crate 只负责把本轮 prompt 写入唯一的 Web
//! 槽位并读取回复。工具完全走标准 Magi MCP 服务（槽位端点 + OpenAI Tunnel），
//! 不再有 T2 文本协议或 Web 侧上下文重放。

mod binding;
mod channel;
mod client;
mod driver;
mod errors;
mod factory;
mod images;
mod runtime;
mod site;
pub mod tunnel;

pub use binding::{
    SlotEndHook, WebConversationBinding, WebConversationMode, WebConversationSyncState,
    WebSlotClaim, WebSlotOwner, WebSlotSnapshot, WebSlotTable, WebTurnLease,
};
pub use channel::{WebModelChannelState, WebModelChannelStatus, status_from_tunnel};
pub use client::{
    BrowserWebModelBridgeClient, WebModelClientConfig, WebModelIdentity, normalize_composer_text,
};
pub use driver::{
    ConnectorConfigOutcome, ConnectorStatus, DriverFuture, ImageChunk, LoginState,
    SavedConversationEntry, SavedConversationSink, SavedConversationSnapshot, SavedProgress,
    SubmitOutcome, TurnState, WebMessage, WebModelPageDriver, WriteOutcome,
};
pub use errors::{EngineState, WebModelError, WebModelErrorCode};
pub use factory::{WebModelClientFactory, WebModelInvocationSpec, new_web_slot_table};
pub use images::{
    PageImageRef, WebImage, WebImageSink, page_image_refs, rewrite_page_images, strip_page_images,
};
pub use runtime::{WebModelRuntimeEntry, WebModelRuntimeRegistry};
pub use site::{
    CHATGPT_WEB_HOME_URL, CHATGPT_WEB_ORIGIN, CHATGPT_WEB_TEMPORARY_CHAT_URL, WEB_MODEL_ENGINE_ID,
    chatgpt_web_home_url, chatgpt_web_origin, chatgpt_web_temporary_chat_url,
    is_chatgpt_web_engine_id, openai_platform_api_keys_url, openai_platform_tunnels_url,
    web_model_engine_id,
};
pub use tunnel::{TUNNEL_CLIENT_VERSION, TunnelClientStatus, TunnelManager, TunnelRuntimeConfig};
