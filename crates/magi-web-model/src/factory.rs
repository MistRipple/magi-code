//! GPT Web client 的构造契约。

use std::sync::Arc;

use magi_bridge_client::ModelBridgeClient;

use crate::binding::{WebConversationMode, WebSlotTable};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebModelInvocationSpec {
    pub session_id: String,
    pub project_id: String,
    pub thread_id: String,
    pub engine_id: String,
    pub effort: String,
    pub mode: WebConversationMode,
    pub remote_conversation_id: Option<String>,
}

pub trait WebModelClientFactory: Send + Sync {
    fn build_web_model_client(
        &self,
        spec: WebModelInvocationSpec,
    ) -> Result<Arc<dyn ModelBridgeClient>, String>;
}

/// 供宿主装配使用的共享应用级槽位。
pub fn new_web_slot_table() -> Arc<WebSlotTable> {
    Arc::new(WebSlotTable::new())
}
