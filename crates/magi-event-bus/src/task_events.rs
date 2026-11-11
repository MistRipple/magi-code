use crate::EventEnvelope;
use magi_core::EventId;
use serde_json::json;
// --- Task submission lifecycle

pub const TASK_SUBMISSION_CREATED: &str = "task.submission.created";
// --- Task status transitions

pub const TASK_STATUS_CHANGED: &str = "task.status.changed";
// --- Helper functions — build properly structured EventEnvelope instances

/// Create a domain event for a task status transition.
#[cfg(test)]
pub(crate) fn task_status_changed_event(
    task_id: &str,
    mission_id: &str,
    old_status: &str,
    new_status: &str,
    kind: &str,
) -> EventEnvelope {
    EventEnvelope::domain(
        EventId::unique("event-task-status-changed"),
        TASK_STATUS_CHANGED,
        json!({
            "task_id": task_id,
            "mission_id": mission_id,
            "old_status": old_status,
            "new_status": new_status,
            "kind": kind,
        }),
    )
}

/// Create a domain event for a newly created task submission.
pub fn task_submission_created_event(
    mission_id: &str,
    root_task_id: &str,
    task_count: usize,
) -> EventEnvelope {
    EventEnvelope::domain(
        EventId::unique("event-task-submission-created"),
        TASK_SUBMISSION_CREATED,
        json!({
            "mission_id": mission_id,
            "root_task_id": root_task_id,
            "task_count": task_count,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventCategory, InMemoryEventBus};

    #[test]
    fn task_status_changed_event_creates_valid_envelope() {
        let event =
            task_status_changed_event("task-001", "mission-001", "pending", "running", "execution");
        assert_eq!(event.event_type, TASK_STATUS_CHANGED);
        assert_eq!(event.category, EventCategory::Domain);
        assert!(
            event
                .event_id
                .as_str()
                .starts_with("event-task-status-changed-")
        );
        assert_eq!(event.payload["task_id"], "task-001");
        assert_eq!(event.payload["mission_id"], "mission-001");
        assert_eq!(event.payload["old_status"], "pending");
        assert_eq!(event.payload["new_status"], "running");
        assert_eq!(event.payload["kind"], "execution");
    }

    #[test]
    fn task_submission_created_event_creates_valid_envelope() {
        let event = task_submission_created_event("mission-001", "root-001", 5);
        assert_eq!(event.event_type, TASK_SUBMISSION_CREATED);
        assert_eq!(event.category, EventCategory::Domain);
        assert!(
            event
                .event_id
                .as_str()
                .starts_with("event-task-submission-created-")
        );
        assert_eq!(event.payload["mission_id"], "mission-001");
        assert_eq!(event.payload["root_task_id"], "root-001");
        assert_eq!(event.payload["task_count"], 5);
    }

    #[test]
    fn task_events_can_be_published_through_event_bus() {
        let bus = InMemoryEventBus::new(16);
        let _receiver = bus.subscribe();

        let events = vec![
            task_status_changed_event("task-001", "mission-001", "pending", "running", "execution"),
            task_submission_created_event("mission-001", "root-001", 3),
        ];

        for event in &events {
            let sequence = bus.publish(event.clone());
            assert!(sequence > 0);
        }

        let snapshot = bus.snapshot();
        assert_eq!(snapshot.recent_events.len(), events.len());
        assert_eq!(snapshot.recent_events[0].event_type, TASK_STATUS_CHANGED);
        assert_eq!(
            snapshot.recent_events[1].event_type,
            TASK_SUBMISSION_CREATED
        );
    }
}
