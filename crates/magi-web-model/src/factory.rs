//! GPT Web client 的构造契约。

use std::sync::Arc;

use magi_bridge_client::ModelBridgeClient;

use crate::binding::{WebConversationBinding, WebSlotTable};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebModelInvocationSpec {
    pub session_id: String,
    pub project_id: String,
    pub thread_id: String,
    pub engine_id: String,
    /// 会话级 Web 对话绑定（临时 / 已保存及远端引用）。
    pub binding: WebConversationBinding,
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
