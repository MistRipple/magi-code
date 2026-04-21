use magi_core::{AssignmentId, EventId, MissionId, SessionId, TaskId, UtcMillis, WorkspaceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventCategory {
    Domain,
    Audit,
    Usage,
    Projection,
    System,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event_id: EventId,
    pub event_type: String,
    pub category: EventCategory,
    pub occurred_at: UtcMillis,
    pub sequence: u64,
    pub workspace_id: Option<WorkspaceId>,
    pub session_id: Option<SessionId>,
    pub mission_id: Option<MissionId>,
    pub assignment_id: Option<AssignmentId>,
    pub task_id: Option<TaskId>,
    pub payload: Value,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventContext {
    pub workspace_id: Option<WorkspaceId>,
    pub session_id: Option<SessionId>,
    pub mission_id: Option<MissionId>,
    pub assignment_id: Option<AssignmentId>,
    pub task_id: Option<TaskId>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EventStreamSnapshot {
    pub next_sequence: u64,
    pub recent_events: Vec<EventEnvelope>,
}

impl EventEnvelope {
    pub fn domain(event_id: EventId, event_type: impl Into<String>, payload: Value) -> Self {
        Self {
            event_id,
            event_type: event_type.into(),
            category: EventCategory::Domain,
            occurred_at: UtcMillis::now(),
            sequence: 0,
            workspace_id: None,
            session_id: None,
            mission_id: None,
            assignment_id: None,
            task_id: None,
            payload,
        }
    }

    pub fn audit(event_id: EventId, event_type: impl Into<String>, payload: Value) -> Self {
        Self {
            event_id,
            event_type: event_type.into(),
            category: EventCategory::Audit,
            occurred_at: UtcMillis::now(),
            sequence: 0,
            workspace_id: None,
            session_id: None,
            mission_id: None,
            assignment_id: None,
            task_id: None,
            payload,
        }
    }

    pub fn usage(event_id: EventId, event_type: impl Into<String>, payload: Value) -> Self {
        Self {
            event_id,
            event_type: event_type.into(),
            category: EventCategory::Usage,
            occurred_at: UtcMillis::now(),
            sequence: 0,
            workspace_id: None,
            session_id: None,
            mission_id: None,
            assignment_id: None,
            task_id: None,
            payload,
        }
    }

    pub fn system(event_id: EventId, event_type: impl Into<String>, payload: Value) -> Self {
        Self {
            event_id,
            event_type: event_type.into(),
            category: EventCategory::System,
            occurred_at: UtcMillis::now(),
            sequence: 0,
            workspace_id: None,
            session_id: None,
            mission_id: None,
            assignment_id: None,
            task_id: None,
            payload,
        }
    }

    pub fn projection(event_id: EventId, event_type: impl Into<String>, payload: Value) -> Self {
        Self {
            event_id,
            event_type: event_type.into(),
            category: EventCategory::Projection,
            occurred_at: UtcMillis::now(),
            sequence: 0,
            workspace_id: None,
            session_id: None,
            mission_id: None,
            assignment_id: None,
            task_id: None,
            payload,
        }
    }

    pub fn with_context(mut self, context: EventContext) -> Self {
        self.workspace_id = context.workspace_id;
        self.session_id = context.session_id;
        self.mission_id = context.mission_id;
        self.assignment_id = context.assignment_id;
        self.task_id = context.task_id;
        self
    }
}
