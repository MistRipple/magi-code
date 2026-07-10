use crate::EventEnvelope;
use magi_core::{EventId, UtcMillis};
use serde_json::json;
// --- Task submission lifecycle

pub const TASK_SUBMISSION_CREATED: &str = "task.submission.created";
// --- Task status transitions

pub const TASK_STATUS_CHANGED: &str = "task.status.changed";
pub const TASK_RUNNING: &str = "task.running";
pub const TASK_COMPLETED: &str = "task.completed";
pub const TASK_FAILED: &str = "task.failed";
pub const TASK_KILLED: &str = "task.killed";
// --- Lease lifecycle

pub const LEASE_GRANTED: &str = "task.lease.granted";
pub const LEASE_COMPLETED: &str = "task.lease.completed";
pub const LEASE_EXPIRED: &str = "task.lease.expired";
pub const LEASE_REVOKED: &str = "task.lease.revoked";
// --- Checkpoint

pub const CHECKPOINT_SAVED: &str = "task.checkpoint.saved";
pub const CHECKPOINT_RESTORED: &str = "task.checkpoint.restored";

// --- Helper functions — build properly structured EventEnvelope instances

/// Create a domain event for a task status transition.
pub fn task_status_changed_event(
    task_id: &str,
    mission_id: &str,
    old_status: &str,
    new_status: &str,
    kind: &str,
) -> EventEnvelope {
    EventEnvelope::domain(
        EventId::new(format!("event-task-status-changed-{}", UtcMillis::now().0)),
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
        EventId::new(format!(
            "event-task-submission-created-{}",
            UtcMillis::now().0
        )),
        TASK_SUBMISSION_CREATED,
        json!({
            "mission_id": mission_id,
            "root_task_id": root_task_id,
            "task_count": task_count,
        }),
    )
}

/// Create a domain event when a lease is granted to a worker.
pub fn lease_granted_event(
    lease_id: &str,
    task_id: &str,
    worker_id: &str,
    role: &str,
) -> EventEnvelope {
    EventEnvelope::domain(
        EventId::new(format!("event-task-lease-granted-{}", UtcMillis::now().0)),
        LEASE_GRANTED,
        json!({
            "lease_id": lease_id,
            "task_id": task_id,
            "worker_id": worker_id,
            "role": role,
        }),
    )
}

/// Create a domain event when a lease is completed.
pub fn lease_completed_event(lease_id: &str, task_id: &str, worker_id: &str) -> EventEnvelope {
    EventEnvelope::domain(
        EventId::new(format!("event-task-lease-completed-{}", UtcMillis::now().0)),
        LEASE_COMPLETED,
        json!({
            "lease_id": lease_id,
            "task_id": task_id,
            "worker_id": worker_id,
        }),
    )
}

/// Create a domain event when a lease expires.
pub fn lease_expired_event(lease_id: &str, task_id: &str, worker_id: &str) -> EventEnvelope {
    EventEnvelope::domain(
        EventId::new(format!("event-task-lease-expired-{}", UtcMillis::now().0)),
        LEASE_EXPIRED,
        json!({
            "lease_id": lease_id,
            "task_id": task_id,
            "worker_id": worker_id,
        }),
    )
}

/// Create a domain event when a lease is revoked.
pub fn lease_revoked_event(
    lease_id: &str,
    task_id: &str,
    worker_id: &str,
    reason: &str,
) -> EventEnvelope {
    EventEnvelope::domain(
        EventId::new(format!("event-task-lease-revoked-{}", UtcMillis::now().0)),
        LEASE_REVOKED,
        json!({
            "lease_id": lease_id,
            "task_id": task_id,
            "worker_id": worker_id,
            "reason": reason,
        }),
    )
}

/// Create a domain event when a checkpoint is saved.
pub fn checkpoint_saved_event(
    checkpoint_id: &str,
    task_id: &str,
    mission_id: &str,
) -> EventEnvelope {
    EventEnvelope::domain(
        EventId::new(format!(
            "event-task-checkpoint-saved-{}",
            UtcMillis::now().0
        )),
        CHECKPOINT_SAVED,
        json!({
            "checkpoint_id": checkpoint_id,
            "task_id": task_id,
            "mission_id": mission_id,
        }),
    )
}

/// Create a domain event when a checkpoint is restored.
pub fn checkpoint_restored_event(
    checkpoint_id: &str,
    task_id: &str,
    mission_id: &str,
) -> EventEnvelope {
    EventEnvelope::domain(
        EventId::new(format!(
            "event-task-checkpoint-restored-{}",
            UtcMillis::now().0
        )),
        CHECKPOINT_RESTORED,
        json!({
            "checkpoint_id": checkpoint_id,
            "task_id": task_id,
            "mission_id": mission_id,
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
    fn lease_granted_event_creates_valid_envelope() {
        let event = lease_granted_event("lease-001", "task-001", "worker-001", "executor");
        assert_eq!(event.event_type, LEASE_GRANTED);
        assert_eq!(event.category, EventCategory::Domain);
        assert!(
            event
                .event_id
                .as_str()
                .starts_with("event-task-lease-granted-")
        );
        assert_eq!(event.payload["lease_id"], "lease-001");
        assert_eq!(event.payload["task_id"], "task-001");
        assert_eq!(event.payload["worker_id"], "worker-001");
        assert_eq!(event.payload["role"], "executor");
    }

    #[test]
    fn lease_completed_event_creates_valid_envelope() {
        let event = lease_completed_event("lease-001", "task-001", "worker-001");
        assert_eq!(event.event_type, LEASE_COMPLETED);
        assert_eq!(event.category, EventCategory::Domain);
        assert_eq!(event.payload["lease_id"], "lease-001");
        assert_eq!(event.payload["task_id"], "task-001");
        assert_eq!(event.payload["worker_id"], "worker-001");
    }

    #[test]
    fn lease_expired_event_creates_valid_envelope() {
        let event = lease_expired_event("lease-002", "task-002", "worker-002");
        assert_eq!(event.event_type, LEASE_EXPIRED);
        assert_eq!(event.category, EventCategory::Domain);
        assert_eq!(event.payload["lease_id"], "lease-002");
        assert_eq!(event.payload["task_id"], "task-002");
        assert_eq!(event.payload["worker_id"], "worker-002");
    }

    #[test]
    fn lease_revoked_event_creates_valid_envelope() {
        let event = lease_revoked_event("lease-003", "task-003", "worker-003", "timeout");
        assert_eq!(event.event_type, LEASE_REVOKED);
        assert_eq!(event.category, EventCategory::Domain);
        assert_eq!(event.payload["lease_id"], "lease-003");
        assert_eq!(event.payload["task_id"], "task-003");
        assert_eq!(event.payload["worker_id"], "worker-003");
        assert_eq!(event.payload["reason"], "timeout");
    }

    #[test]
    fn checkpoint_saved_event_creates_valid_envelope() {
        let event = checkpoint_saved_event("cp-001", "task-001", "mission-001");
        assert_eq!(event.event_type, CHECKPOINT_SAVED);
        assert_eq!(event.category, EventCategory::Domain);
        assert_eq!(event.payload["checkpoint_id"], "cp-001");
        assert_eq!(event.payload["task_id"], "task-001");
        assert_eq!(event.payload["mission_id"], "mission-001");
    }

    #[test]
    fn checkpoint_restored_event_creates_valid_envelope() {
        let event = checkpoint_restored_event("cp-001", "task-001", "mission-001");
        assert_eq!(event.event_type, CHECKPOINT_RESTORED);
        assert_eq!(event.category, EventCategory::Domain);
        assert_eq!(event.payload["checkpoint_id"], "cp-001");
        assert_eq!(event.payload["task_id"], "task-001");
        assert_eq!(event.payload["mission_id"], "mission-001");
    }

    #[test]
    fn task_events_can_be_published_through_event_bus() {
        let bus = InMemoryEventBus::new(16);
        let _receiver = bus.subscribe();

        let events = vec![
            task_status_changed_event("task-001", "mission-001", "pending", "running", "execution"),
            task_submission_created_event("mission-001", "root-001", 3),
            lease_granted_event("lease-001", "task-001", "worker-001", "executor"),
            checkpoint_saved_event("cp-001", "task-001", "mission-001"),
        ];

        for event in &events {
            let sequence = bus
                .publish(event.clone())
                .expect("publish task event should succeed");
            assert!(sequence > 0);
        }

        let snapshot = bus.snapshot();
        assert_eq!(snapshot.recent_events.len(), events.len());
        assert_eq!(snapshot.recent_events[0].event_type, TASK_STATUS_CHANGED);
        assert_eq!(
            snapshot.recent_events[1].event_type,
            TASK_SUBMISSION_CREATED
        );
        assert_eq!(snapshot.recent_events[2].event_type, LEASE_GRANTED);
        assert_eq!(snapshot.recent_events[3].event_type, CHECKPOINT_SAVED);
    }

    #[test]
    fn all_event_type_constants_follow_naming_convention() {
        let task_scoped = [
            TASK_SUBMISSION_CREATED,
            TASK_STATUS_CHANGED,
            TASK_RUNNING,
            TASK_COMPLETED,
            TASK_FAILED,
            TASK_KILLED,
            LEASE_GRANTED,
            LEASE_COMPLETED,
            LEASE_EXPIRED,
            LEASE_REVOKED,
            CHECKPOINT_SAVED,
            CHECKPOINT_RESTORED,
        ];
        for event_type in &task_scoped {
            assert!(
                !event_type.contains(' '),
                "event type '{}' should not contain spaces",
                event_type
            );
        }
        for event_type in &task_scoped {
            assert!(
                event_type.starts_with("task."),
                "event type '{}' should start with 'task.'",
                event_type
            );
        }
    }
}
