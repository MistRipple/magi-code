//! Task System v2 - 统一流派生通道（Slice S3）。
//!
//! 模型 token、工具事件、系统信号在 v1 各自走 callback：
//! `task_llm_loop::publish_stream_delta` / `publish_task_thinking_delta` /
//! `dispatch_execution` 中的 `_on_delta` 闭包。v2 把它们收口为同一个
//! `StreamEvent` 派生源，下游（writeback / lane summary / projection）
//! 各自订阅自己关心的子集。
//!
//! S3 范围内仅定义类型与最小 fan-out 协议；v1 publish 路径仍是真实 IO 落点，
//! 由后续 slice 把现有 callback 替换为往这里推。

use std::sync::mpsc::{Receiver, Sender, channel};

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

/// 一个最小的 fan-out 派生通道：写端 publish 一份，读端按 SessionId 过滤。
/// S3 范围内是单写多读的同步 channel——不引入 tokio broadcast 等运行时绑定。
#[derive(Debug)]
pub struct StreamFanOut {
    senders: Vec<Sender<StreamEvent>>,
}

impl StreamFanOut {
    pub fn new() -> Self {
        Self {
            senders: Vec::new(),
        }
    }

    pub fn subscribe(&mut self) -> Receiver<StreamEvent> {
        let (tx, rx) = channel();
        self.senders.push(tx);
        rx
    }

    /// publish 失败的订阅者（receiver 已丢弃）会被静默忽略；S3 不引入背压。
    pub fn publish(&mut self, event: StreamEvent) {
        self.senders.retain(|sender| sender.send(event.clone()).is_ok());
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

    fn sample_delta(session: &str, text: &str) -> StreamEvent {
        StreamEvent::ModelDelta {
            session_id: SessionId::new(session),
            content: text.to_string(),
            thinking: String::new(),
        }
    }

    #[test]
    fn single_subscriber_receives_in_order() {
        let mut fan = StreamFanOut::new();
        let rx = fan.subscribe();
        fan.publish(sample_delta("s", "a"));
        fan.publish(sample_delta("s", "b"));
        let collected: Vec<_> = (0..2).map(|_| rx.recv().unwrap()).collect();
        assert!(matches!(&collected[0], StreamEvent::ModelDelta { content, .. } if content == "a"));
        assert!(matches!(&collected[1], StreamEvent::ModelDelta { content, .. } if content == "b"));
    }

    #[test]
    fn multiple_subscribers_each_receive_full_stream() {
        let mut fan = StreamFanOut::new();
        let a = fan.subscribe();
        let b = fan.subscribe();
        fan.publish(sample_delta("s", "x"));
        assert!(a.recv().is_ok());
        assert!(b.recv().is_ok());
    }

    #[test]
    fn dropped_subscriber_is_cleaned_up_on_next_publish() {
        let mut fan = StreamFanOut::new();
        let _kept = fan.subscribe();
        let drop_me = fan.subscribe();
        drop(drop_me);
        fan.publish(sample_delta("s", "x"));
        assert_eq!(fan.senders.len(), 1);
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
