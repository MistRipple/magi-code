use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerLaneSpec {
    pub lane_id: String,
    pub worker: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub task_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidechainEventType {
    Instruction,
    WorkerOutput,
    ToolCall,
    ToolResult,
    FileChange,
    TaskUpdate,
    Summary,
    Error,
    SupplementaryInstruction,
    Wait,
    Poll,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SidechainEvent {
    pub id: String,
    pub event_type: SidechainEventType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub timestamp: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerLaneProgressSummary {
    pub total_tasks: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_tasks: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_tasks: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerLaneStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Blocked,
    ReviewRequired,
}

impl WorkerLaneStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostDispatchBase {
    pub dispatch_message_id: String,
    pub dispatch_wave_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub source_event_id: String,
    pub version: u32,
    pub anchor_timestamp: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    OrchestratorText {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
    },
    DispatchStarted {
        dispatch_message_id: String,
        dispatch_wave_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mission_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        summary: String,
        lanes: Vec<WorkerLaneSpec>,
        anchor_timestamp: u64,
    },
    WorkerStatusChanged {
        dispatch_message_id: String,
        dispatch_wave_id: String,
        lane_id: String,
        worker: String,
        status: WorkerLaneStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ended_at: Option<u64>,
    },
    WorkerActivity {
        dispatch_message_id: String,
        dispatch_wave_id: String,
        lane_id: String,
        worker: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        live_activity: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_count: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress_summary: Option<WorkerLaneProgressSummary>,
    },
    WorkerSidechainEvent {
        dispatch_message_id: String,
        dispatch_wave_id: String,
        lane_id: String,
        worker: String,
        event: SidechainEvent,
    },
    OrchestratorSummary {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
    },
    VerificationStarted {
        #[serde(flatten)]
        base: PostDispatchBase,
        summary: String,
    },
    VerificationCompleted {
        #[serde(flatten)]
        base: PostDispatchBase,
        status: VerificationEventStatus,
        summary: String,
    },
    ReviewStarted {
        #[serde(flatten)]
        base: PostDispatchBase,
        summary: String,
    },
    ReviewCompleted {
        #[serde(flatten)]
        base: PostDispatchBase,
        status: ReviewEventStatus,
        summary: String,
    },
    DecisionEmitted {
        #[serde(flatten)]
        base: PostDispatchBase,
        decision: Value,
    },
    FinalAnswerEmitted {
        #[serde(flatten)]
        base: PostDispatchBase,
        final_answer: Value,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationEventStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEventStatus {
    Approved,
    NeedsRevision,
    Rejected,
    Skipped,
}

impl RuntimeEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::OrchestratorText { .. } => "orchestrator_text",
            Self::DispatchStarted { .. } => "dispatch_started",
            Self::WorkerStatusChanged { .. } => "worker_status_changed",
            Self::WorkerActivity { .. } => "worker_activity",
            Self::WorkerSidechainEvent { .. } => "worker_sidechain_event",
            Self::OrchestratorSummary { .. } => "orchestrator_summary",
            Self::VerificationStarted { .. } => "verification_started",
            Self::VerificationCompleted { .. } => "verification_completed",
            Self::ReviewStarted { .. } => "review_started",
            Self::ReviewCompleted { .. } => "review_completed",
            Self::DecisionEmitted { .. } => "decision_emitted",
            Self::FinalAnswerEmitted { .. } => "final_answer_emitted",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_lane_status_terminal() {
        assert!(WorkerLaneStatus::Completed.is_terminal());
        assert!(WorkerLaneStatus::Failed.is_terminal());
        assert!(WorkerLaneStatus::Cancelled.is_terminal());
        assert!(!WorkerLaneStatus::Running.is_terminal());
        assert!(!WorkerLaneStatus::Pending.is_terminal());
        assert!(!WorkerLaneStatus::Blocked.is_terminal());
    }

    #[test]
    fn event_type_names() {
        let event = RuntimeEvent::OrchestratorText {
            content: "hello".to_string(),
            metadata: None,
        };
        assert_eq!(event.event_type(), "orchestrator_text");

        let event = RuntimeEvent::DispatchStarted {
            dispatch_message_id: "msg-1".to_string(),
            dispatch_wave_id: "wave-1".to_string(),
            session_id: None,
            mission_id: None,
            turn_id: None,
            request_id: None,
            summary: "test".to_string(),
            lanes: vec![],
            anchor_timestamp: 0,
        };
        assert_eq!(event.event_type(), "dispatch_started");
    }

    #[test]
    fn sidechain_event_serializes() {
        let event = SidechainEvent {
            id: "ev-1".to_string(),
            event_type: SidechainEventType::ToolCall,
            content: Some("read_file".to_string()),
            timestamp: 1000,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["eventType"], "tool_call");
        assert_eq!(json["content"], "read_file");
    }

    #[test]
    fn runtime_event_serializes_tagged() {
        let event = RuntimeEvent::WorkerStatusChanged {
            dispatch_message_id: "msg-1".to_string(),
            dispatch_wave_id: "wave-1".to_string(),
            lane_id: "lane-1".to_string(),
            worker: "worker-a".to_string(),
            status: WorkerLaneStatus::Running,
            title: Some("task title".to_string()),
            description: None,
            started_at: Some(1000),
            ended_at: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "worker_status_changed");
        assert_eq!(json["status"], "running");
        assert_eq!(json["worker"], "worker-a");
    }

    #[test]
    fn worker_lane_spec_roundtrip() {
        let spec = WorkerLaneSpec {
            lane_id: "lane-1".to_string(),
            worker: "worker-a".to_string(),
            title: "task".to_string(),
            description: None,
            task_ids: vec!["t1".to_string(), "t2".to_string()],
        };
        let json = serde_json::to_string(&spec).unwrap();
        let restored: WorkerLaneSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.lane_id, "lane-1");
        assert_eq!(restored.task_ids.len(), 2);
    }

    #[test]
    fn verification_status_values() {
        assert_eq!(
            serde_json::to_value(VerificationEventStatus::Passed).unwrap(),
            "passed"
        );
        assert_eq!(
            serde_json::to_value(ReviewEventStatus::NeedsRevision).unwrap(),
            "needs_revision"
        );
    }
}
