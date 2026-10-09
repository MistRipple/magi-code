//! GPT Web client 的构造契约。

use std::sync::Arc;

use crate::binding::WebSlotTable;

/// 供宿主装配使用的共享应用级槽位。
pub fn new_web_slot_table() -> Arc<WebSlotTable> {
    Arc::new(WebSlotTable::new())
}
