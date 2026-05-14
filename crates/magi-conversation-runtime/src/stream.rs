//! Task System v2 - 统一流派生通道（Slice S3）。
//!
//! 模型 token、工具事件、系统信号在 v1 各自走 callback：
//! `task_llm_loop::publish_stream_delta` / `publish_task_thinking_delta` /
//! `dispatch_execution` 中的 `_on_delta` 闭包。v2 把它们收口为同一个
//! `StreamEvent` 派生源，下游（writeback / lane summary / projection /
//! UI bridge）各自注册自己的回调订阅。
//!
//! S3 范围：
//! - 类型契约：`StreamEvent` + `ToolPhase`
//! - 派生通道：callback-based `StreamFanOut`（同步扇出，不引入 tokio/mpsc）
//! - v1 publish 段照原样保留状态写入（session_store 等），增量把 fanout 作为
//!   observation 通道暴露出来，下游 UI bridge 后续 slice 切换到这里订阅
//!   并删除 event_bus 上对应的 session_turn_item_event 分支
//!
//! 同步扇出选择：模型 IO 是单调度线程，发布回调直接在生产者线程跑；
//! 不需要 mpsc receiver thread，也避免 std::sync::mpsc 在 fan-out 取消订阅
//! 时的复杂性。后续慢消费者由订阅者自行决定是否 spawn 处理。

use std::sync::{Arc, Mutex};

use magi_core::{SessionId, ToolCallId};
use serde::{Deserialize, Serialize};

/// 流上的一个事件。tag 形态以方便后续 SSE/JSONL 透传。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamEvent {
    /// LLM 输出 token delta。`thinking` 与 `content` 是 v1 ModelStreamingDelta
    /// 的两个独立通道；前端 thinking 摘要和正文渲染走不同的 reducer。
    ModelDelta {
        session_id: SessionId,
        content: String,
        thinking: String,
    },
    /// 工具调用开始/结束（结束携带最终结果文本）。
    ToolEvent {
        session_id: SessionId,
        tool_call_id: ToolCallId,
        phase: ToolPhase,
        payload: String,
    },
    /// 系统级状态信号（lease 失效、Turn 转换、错误降级等）。
    SystemSignal {
        session_id: SessionId,
        code: String,
        detail: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPhase {
    Started,
    Succeeded,
    Failed,
}

impl StreamEvent {
    pub fn session_id(&self) -> &SessionId {
        match self {
            Self::ModelDelta { session_id, .. }
            | Self::ToolEvent { session_id, .. }
            | Self::SystemSignal { session_id, .. } => session_id,
        }
    }
}

/// 同步 fan-out：每个订阅者是一个 `Fn(&StreamEvent)` 闭包，publish 时按
/// 注册顺序在生产者线程依次回调。订阅者持有自己的状态（通常通过 `Arc<Mutex<_>>`
/// 捕获）；不在 fanout 层做线程切换。
///
/// 订阅者通过 `subscribe` 拿到一个 `SubscriptionId`，可以稍后 `unsubscribe`。
/// 在 LLM 调用 / 工具批 / Turn 期间临时挂入的订阅者用 RAII guard 自动撤销，
/// 由调用方包装；fanout 本身不负责 guard 寿命。
pub struct StreamFanOut {
    inner: Mutex<StreamFanOutInner>,
}

struct StreamFanOutInner {
    next_id: u64,
    subscribers: Vec<(SubscriptionId, Subscriber)>,
}

type Subscriber = Arc<dyn Fn(&StreamEvent) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(u64);

impl StreamFanOut {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(StreamFanOutInner {
                next_id: 1,
                subscribers: Vec::new(),
            }),
        }
    }

    pub fn subscribe<F>(&self, callback: F) -> SubscriptionId
    where
        F: Fn(&StreamEvent) + Send + Sync + 'static,
    {
        let mut guard = self.inner.lock().expect("StreamFanOut mutex poisoned");
        let id = SubscriptionId(guard.next_id);
        guard.next_id += 1;
        guard.subscribers.push((id, Arc::new(callback)));
        id
    }

    pub fn unsubscribe(&self, id: SubscriptionId) -> bool {
        let mut guard = self.inner.lock().expect("StreamFanOut mutex poisoned");
        let before = guard.subscribers.len();
        guard.subscribers.retain(|(existing, _)| *existing != id);
        guard.subscribers.len() < before
    }

    pub fn publish(&self, event: StreamEvent) {
        let subs: Vec<Subscriber> = {
            let guard = self.inner.lock().expect("StreamFanOut mutex poisoned");
            guard
                .subscribers
                .iter()
                .map(|(_, callback)| Arc::clone(callback))
                .collect()
        };
        for callback in subs {
            callback(&event);
        }
    }

    pub fn subscriber_count(&self) -> usize {
        self.inner
            .lock()
            .expect("StreamFanOut mutex poisoned")
            .subscribers
            .len()
    }
}

impl std::fmt::Debug for StreamFanOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamFanOut")
            .field("subscribers", &self.subscriber_count())
            .finish()
    }
}

impl Default for StreamFanOut {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn sample_delta(session: &str, text: &str) -> StreamEvent {
        StreamEvent::ModelDelta {
            session_id: SessionId::new(session),
            content: text.to_string(),
            thinking: String::new(),
        }
    }

    #[test]
    fn single_subscriber_receives_in_order() {
        let fan = StreamFanOut::new();
        let log: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let log_clone = Arc::clone(&log);
        fan.subscribe(move |evt| {
            if let StreamEvent::ModelDelta { content, .. } = evt {
                log_clone.lock().unwrap().push(content.clone());
            }
        });
        fan.publish(sample_delta("s", "a"));
        fan.publish(sample_delta("s", "b"));
        let collected = log.lock().unwrap().clone();
        assert_eq!(collected, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn multiple_subscribers_each_receive_full_stream() {
        let fan = StreamFanOut::new();
        let a: Arc<StdMutex<usize>> = Arc::new(StdMutex::new(0));
        let b: Arc<StdMutex<usize>> = Arc::new(StdMutex::new(0));
        let a2 = Arc::clone(&a);
        let b2 = Arc::clone(&b);
        fan.subscribe(move |_| *a2.lock().unwrap() += 1);
        fan.subscribe(move |_| *b2.lock().unwrap() += 1);
        fan.publish(sample_delta("s", "x"));
        fan.publish(sample_delta("s", "y"));
        assert_eq!(*a.lock().unwrap(), 2);
        assert_eq!(*b.lock().unwrap(), 2);
    }

    #[test]
    fn unsubscribe_stops_callbacks() {
        let fan = StreamFanOut::new();
        let counter: Arc<StdMutex<usize>> = Arc::new(StdMutex::new(0));
        let counter2 = Arc::clone(&counter);
        let id = fan.subscribe(move |_| *counter2.lock().unwrap() += 1);
        fan.publish(sample_delta("s", "first"));
        assert!(fan.unsubscribe(id));
        fan.publish(sample_delta("s", "second"));
        assert_eq!(*counter.lock().unwrap(), 1);
    }

    #[test]
    fn unsubscribe_unknown_returns_false() {
        let fan = StreamFanOut::new();
        assert!(!fan.unsubscribe(SubscriptionId(999)));
    }

    #[test]
    fn tool_event_routes_session_id() {
        let evt = StreamEvent::ToolEvent {
            session_id: SessionId::new("S"),
            tool_call_id: ToolCallId::new("call-1"),
            phase: ToolPhase::Started,
            payload: String::new(),
        };
        assert_eq!(evt.session_id().as_str(), "S");
    }
}
