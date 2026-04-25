use magi_bridge_client::{BridgeServerKind, BridgeTransport};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct BridgeTransportBinding {
    pub(super) server_kind: BridgeServerKind,
    pub(super) transport: Arc<dyn BridgeTransport>,
}

pub(super) fn excerpt(value: &str) -> String {
    let mut chars = value.chars();
    let excerpt: String = chars.by_ref().take(120).collect();
    if chars.next().is_some() {
        format!("{excerpt}...")
    } else {
        excerpt
    }
}
