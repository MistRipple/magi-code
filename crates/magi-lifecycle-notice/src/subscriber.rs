//! Event bus 订阅器：把 mission 生命周期事件喂给 `LifecycleNoticeRegistry`。
//!
//! 跑在 tokio task 里：当 broadcast sender 全部被 drop（daemon 退出时
//! `Arc<InMemoryEventBus>` 引用归零）就自然结束，无需显式取消信号。
//! lagged 时 warn 但不退出（避免一条事件错过就关闭整路通道）。
//!
//! daemon bootstrap：
//! ```ignore
//! let registry = Arc::new(LifecycleNoticeRegistry::new());
//! tokio::spawn(run_subscriber(registry.clone(), event_bus.clone()));
//! ```

use std::sync::Arc;

use magi_event_bus::InMemoryEventBus;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, warn};

use crate::notice_queue::LifecycleNoticeRegistry;

pub async fn run_subscriber(
    registry: Arc<LifecycleNoticeRegistry>,
    event_bus: Arc<InMemoryEventBus>,
) {
    let mut receiver = event_bus.subscribe();
    loop {
        match receiver.recv().await {
            Ok(env) => registry.ingest(&env),
            Err(RecvError::Lagged(skipped)) => {
                warn!(
                    skipped,
                    "lifecycle-notice subscriber lagged，部分事件被跳过"
                );
            }
            Err(RecvError::Closed) => {
                debug!("lifecycle-notice subscriber 发现 sender 已关闭，退出");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::MissionId;
    use magi_event_bus::EventContext;
    use magi_event_bus::task_events::mission_resumed_from_recovery_event;

    #[tokio::test]
    async fn subscriber_ingests_published_events() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let registry = Arc::new(LifecycleNoticeRegistry::new());
        let handle = tokio::spawn(run_subscriber(registry.clone(), bus.clone()));

        // 给订阅者时间 subscribe()。
        tokio::task::yield_now().await;

        let env = mission_resumed_from_recovery_event("M-sub", "rec-1", 2, None, None)
            .with_context(EventContext {
                mission_id: Some(MissionId::new("M-sub")),
                ..EventContext::default()
            });
        bus.publish(env).expect("publish");

        // 让 subscriber poll 一次。
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let body = registry
            .pending_notice(&MissionId::new("M-sub"))
            .expect("subscriber 应已注入 notice");
        assert!(body.contains("rec-1"));

        // drop sender → subscriber 自然结束。
        drop(bus);
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), handle).await;
    }
}
