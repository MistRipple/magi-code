use magi_core::{EventId, MissionId, SessionId, TaskId, UtcMillis, WorkerId};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorMessageKind {
    DispatchStarted,
    DispatchCompleted,
    DispatchFailed,
    DispatchCancelled,
    WorkerStarted,
    WorkerCompleted,
    WorkerFailed,
    MissionCreated,
    MissionCompleted,
    MissionFailed,
    ProgressUpdate,
    ErrorReport,
    VerificationResult,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestratorMessage {
    pub kind: OrchestratorMessageKind,
    pub message: String,
    pub context: MessageContext,
    pub payload: Value,
    pub occurred_at: UtcMillis,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MessageContext {
    pub session_id: Option<SessionId>,
    pub mission_id: Option<MissionId>,
    pub task_id: Option<TaskId>,
    pub worker_id: Option<WorkerId>,
}

impl MessageContext {
    pub fn for_mission(mission_id: MissionId) -> Self {
        Self {
            mission_id: Some(mission_id),
            ..Default::default()
        }
    }

    pub fn for_task(mission_id: MissionId, task_id: TaskId) -> Self {
        Self {
            mission_id: Some(mission_id),
            task_id: Some(task_id),
            ..Default::default()
        }
    }

    pub fn for_worker(mission_id: MissionId, task_id: TaskId, worker_id: WorkerId) -> Self {
        Self {
            mission_id: Some(mission_id),
            task_id: Some(task_id),
            worker_id: Some(worker_id),
            ..Default::default()
        }
    }

    fn to_event_context(&self) -> EventContext {
        EventContext {
            session_id: self.session_id.clone(),
            mission_id: self.mission_id.clone(),
            task_id: self.task_id.clone(),
            ..Default::default()
        }
    }
}

#[derive(Clone)]
pub struct OrchestratorMessageBus {
    event_bus: Arc<InMemoryEventBus>,
}

impl OrchestratorMessageBus {
    pub fn new(event_bus: Arc<InMemoryEventBus>) -> Self {
        Self { event_bus }
    }

    pub fn publish(&self, msg: OrchestratorMessage) -> u64 {
        let event_type = format!("orchestrator.{:?}", msg.kind).to_lowercase();
        let event_id = EventId::new(format!("{}-{}", event_type, msg.occurred_at.0));

        let envelope = EventEnvelope::domain(event_id, &event_type, serde_json::json!({
            "kind": msg.kind,
            "message": msg.message,
            "payload": msg.payload,
        }))
        .with_context(msg.context.to_event_context());

        self.event_bus.publish(envelope).unwrap_or(0)
    }

    pub fn publish_many(&self, messages: Vec<OrchestratorMessage>) -> Vec<u64> {
        messages.into_iter().map(|m| self.publish(m)).collect()
    }

    pub fn event_bus(&self) -> &Arc<InMemoryEventBus> {
        &self.event_bus
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bus() -> OrchestratorMessageBus {
        OrchestratorMessageBus::new(Arc::new(InMemoryEventBus::new(64)))
    }

    #[test]
    fn publish_returns_sequence() {
        let bus = make_bus();
        let msg = OrchestratorMessage {
            kind: OrchestratorMessageKind::MissionCreated,
            message: "任务创建".to_string(),
            context: MessageContext::for_mission(MissionId::new("m-1")),
            payload: serde_json::json!({}),
            occurred_at: UtcMillis::now(),
        };
        let seq = bus.publish(msg);
        assert!(seq > 0);
    }

    #[test]
    fn publish_many_returns_sequences() {
        let bus = make_bus();
        let messages = vec![
            OrchestratorMessage {
                kind: OrchestratorMessageKind::DispatchStarted,
                message: "开始派发".to_string(),
                context: MessageContext::default(),
                payload: serde_json::json!({}),
                occurred_at: UtcMillis::now(),
            },
            OrchestratorMessage {
                kind: OrchestratorMessageKind::DispatchCompleted,
                message: "派发完成".to_string(),
                context: MessageContext::default(),
                payload: serde_json::json!({}),
                occurred_at: UtcMillis::now(),
            },
        ];
        let seqs = bus.publish_many(messages);
        assert_eq!(seqs.len(), 2);
        assert!(seqs[1] > seqs[0]);
    }

    #[test]
    fn message_context_builders() {
        let ctx = MessageContext::for_worker(
            MissionId::new("m-1"),
            TaskId::new("t-1"),
            WorkerId::new("w-1"),
        );
        assert_eq!(ctx.mission_id.unwrap().to_string(), "m-1");
        assert_eq!(ctx.task_id.unwrap().to_string(), "t-1");
        assert_eq!(ctx.worker_id.unwrap().to_string(), "w-1");
    }
}
