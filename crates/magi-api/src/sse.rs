use axum::{
    http::{HeaderName, HeaderValue, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures_util::{Stream, StreamExt, stream};
use magi_core::{EventId, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
use std::{convert::Infallible, time::Duration};
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};

use crate::public_canonical::public_event_envelope;
use crate::state::ApiState;

pub async fn events(
    state: ApiState,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    session_id: Option<String>,
    after_sequence: Option<u64>,
) -> Response {
    let workspace_id = resolve_event_stream_workspace_id(&state, workspace_id, workspace_path);
    let session_id = resolve_event_stream_session_id(session_id);
    let stream = event_envelope_stream(state, workspace_id, session_id, after_sequence)
        .map(|event| Ok::<Event, Infallible>(event_to_sse(event)));

    let mut response = Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(5))
                .event(keep_alive_sse_event()),
        )
        .into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}

pub(crate) fn resolve_after_sequence(
    query_after_sequence: Option<u64>,
    last_event_id: Option<&str>,
) -> Option<u64> {
    let last_event_sequence = last_event_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0);
    match (query_after_sequence, last_event_sequence) {
        (Some(query), Some(last_event)) => Some(query.max(last_event)),
        (Some(query), None) => Some(query),
        (None, Some(last_event)) => Some(last_event),
        (None, None) => None,
    }
}

fn resolve_event_stream_workspace_id(
    state: &ApiState,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
) -> Option<WorkspaceId> {
    let requested_workspace_id = workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|workspace_id| !workspace_id.is_empty());
    let requested_workspace_path = workspace_path
        .as_deref()
        .map(str::trim)
        .filter(|workspace_path| !workspace_path.is_empty());
    if requested_workspace_id.is_none() && requested_workspace_path.is_none() {
        return None;
    }
    state
        .resolve_workspace_id_from_request(
            requested_workspace_id.map(WorkspaceId::new),
            requested_workspace_path,
        )
        .or_else(|| requested_workspace_id.map(WorkspaceId::new))
        .or_else(|| Some(WorkspaceId::new("__unresolved_event_stream_workspace__")))
}

fn resolve_event_stream_session_id(session_id: Option<String>) -> Option<SessionId> {
    session_id
        .as_deref()
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .map(SessionId::new)
}

fn event_envelope_stream(
    state: ApiState,
    workspace_id: Option<WorkspaceId>,
    session_id: Option<SessionId>,
    after_sequence: Option<u64>,
) -> impl Stream<Item = EventEnvelope> {
    let (snapshot, receiver) = state.event_bus.snapshot_and_subscribe();
    let snapshot_state = state.clone();
    let live_state = state;
    let snapshot_workspace_id = workspace_id.clone();
    let live_workspace_id = workspace_id;
    let snapshot_session_id = session_id.clone();
    let live_session_id = session_id;
    let snapshot_gap_recovery = snapshot_gap_skipped_count(&snapshot, after_sequence)
        .map(|skipped| lagged_recovery_event(skipped, snapshot_workspace_id.as_ref()));
    let recent_stream = stream::iter(snapshot.recent_events.into_iter().filter(move |event| {
        after_sequence.is_none_or(|sequence| event.sequence > sequence)
            && event_matches_scope(
                &snapshot_state,
                event,
                snapshot_workspace_id.as_ref(),
                snapshot_session_id.as_ref(),
            )
    }));
    let live_stream = BroadcastStream::new(receiver).filter_map(move |event| {
        let live_state = live_state.clone();
        let live_workspace_id = live_workspace_id.clone();
        let live_session_id = live_session_id.clone();
        async move {
            match event {
                Ok(envelope) => {
                    let matches_sequence =
                        after_sequence.is_none_or(|sequence| envelope.sequence > sequence);
                    (matches_sequence
                        && event_matches_scope(
                            &live_state,
                            &envelope,
                            live_workspace_id.as_ref(),
                            live_session_id.as_ref(),
                        ))
                    .then_some(envelope)
                }
                Err(BroadcastStreamRecvError::Lagged(skipped)) => {
                    Some(lagged_recovery_event(skipped, live_workspace_id.as_ref()))
                }
            }
        }
    });
    stream::iter(snapshot_gap_recovery)
        .chain(recent_stream)
        .chain(live_stream)
}

fn snapshot_gap_skipped_count(
    snapshot: &magi_event_bus::EventStreamSnapshot,
    after_sequence: Option<u64>,
) -> Option<u64> {
    let after_sequence = after_sequence?;
    let earliest_available_sequence = snapshot
        .recent_events
        .first()
        .map(|event| event.sequence)
        .unwrap_or(snapshot.next_sequence);
    let expected_next_sequence = after_sequence.saturating_add(1);
    if expected_next_sequence < earliest_available_sequence {
        Some(earliest_available_sequence - expected_next_sequence)
    } else {
        None
    }
}

fn event_matches_scope(
    state: &ApiState,
    event: &EventEnvelope,
    requested_workspace_id: Option<&WorkspaceId>,
    requested_session_id: Option<&SessionId>,
) -> bool {
    event_matches_workspace(state, event, requested_workspace_id)
        && event_matches_session(event, requested_session_id)
}

fn event_matches_workspace(
    state: &ApiState,
    event: &EventEnvelope,
    requested_workspace_id: Option<&WorkspaceId>,
) -> bool {
    let Some(requested_workspace_id) = requested_workspace_id else {
        return true;
    };
    if event.workspace_id.as_ref() == Some(requested_workspace_id) {
        return true;
    }
    if event.workspace_id.is_some() {
        return false;
    }
    if event_payload_workspace_matches(event, requested_workspace_id) {
        return true;
    }
    event
        .session_id
        .as_ref()
        .and_then(|session_id| state.session_store.session(session_id))
        .is_some_and(|session| {
            session.workspace_id.as_deref() == Some(requested_workspace_id.as_str())
        })
}

fn event_payload_workspace_matches(
    event: &EventEnvelope,
    requested_workspace_id: &WorkspaceId,
) -> bool {
    event
        .payload
        .get("workspace_id")
        .or_else(|| event.payload.get("workspaceId"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|workspace_id| !workspace_id.is_empty())
        .is_some_and(|workspace_id| workspace_id == requested_workspace_id.as_str())
}

fn event_matches_session(event: &EventEnvelope, requested_session_id: Option<&SessionId>) -> bool {
    let Some(requested_session_id) = requested_session_id else {
        return true;
    };
    if event.event_type == "event.stream.keep_alive" || event.event_type == "event.stream.lagged" {
        return true;
    }
    if event.session_id.as_ref() == Some(requested_session_id) {
        return true;
    }
    if event.session_id.is_some() {
        return false;
    }
    if let Some(payload_session_id) = event
        .payload
        .get("session_id")
        .or_else(|| event.payload.get("sessionId"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
    {
        return payload_session_id == requested_session_id.as_str();
    }
    event.mission_id.is_none() && event.assignment_id.is_none() && event.task_id.is_none()
}

fn event_to_sse(event: EventEnvelope) -> Event {
    let event = public_event_envelope(event);
    let event_sse_id = event_sse_id(&event);
    let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
    let sse_event = Event::default().data(payload);
    if let Some(event_sse_id) = event_sse_id {
        sse_event.id(event_sse_id)
    } else {
        sse_event
    }
}

fn event_sse_id(event: &EventEnvelope) -> Option<String> {
    (event.sequence > 0).then(|| event.sequence.to_string())
}

fn keep_alive_sse_event() -> Event {
    let payload =
        serde_json::to_string(&keep_alive_event_envelope()).unwrap_or_else(|_| "{}".to_string());
    Event::default().data(payload)
}

fn keep_alive_event_envelope() -> EventEnvelope {
    EventEnvelope::system(
        EventId::new("event-stream-keep-alive"),
        "event.stream.keep_alive",
        serde_json::json!({
            "heartbeat": true,
            "transport": "sse",
        }),
    )
}

fn lagged_recovery_event(skipped: u64, workspace_id: Option<&WorkspaceId>) -> EventEnvelope {
    let now = UtcMillis::now();
    EventEnvelope::system(
        EventId::new(format!("event-stream-lagged-{}-{}", now.0, skipped)),
        "event.stream.lagged",
        serde_json::json!({
            "skipped": skipped,
            "recovery": "bootstrap",
            "reason": "broadcast_lagged",
        }),
    )
    .with_context(EventContext {
        workspace_id: workspace_id.cloned(),
        ..EventContext::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{AbsolutePath, EventId, SessionId, TaskId};
    use magi_event_bus::{EventContext, InMemoryEventBus};
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use serde_json::json;
    use std::sync::Arc;

    fn test_state() -> ApiState {
        test_state_with_event_capacity(32)
    }

    fn test_state_with_event_capacity(event_capacity: usize) -> ApiState {
        ApiState::new(
            "magi-sse-test",
            Arc::new(InMemoryEventBus::new(event_capacity)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
    }

    fn workspace_id(id: &str) -> WorkspaceId {
        WorkspaceId::new(id)
    }

    #[tokio::test]
    async fn events_response_disables_proxy_buffering() {
        let response = events(
            test_state(),
            Some("workspace-a".to_string()),
            None,
            None,
            None,
        )
        .await;

        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static("no-cache, no-transform"))
        );
        assert_eq!(
            response
                .headers()
                .get(HeaderName::from_static("x-accel-buffering")),
            Some(&HeaderValue::from_static("no"))
        );
    }

    #[test]
    fn keep_alive_event_is_parseable_event_envelope() {
        let event = keep_alive_event_envelope();

        assert_eq!(event.event_type, "event.stream.keep_alive");
        assert_eq!(event.category, magi_event_bus::EventCategory::System);
        assert_eq!(event.payload["heartbeat"], json!(true));
    }

    #[test]
    fn resolve_after_sequence_prefers_newer_last_event_id_cursor() {
        assert_eq!(resolve_after_sequence(Some(10), Some("12")), Some(12));
        assert_eq!(resolve_after_sequence(Some(12), Some("10")), Some(12));
        assert_eq!(resolve_after_sequence(None, Some("7")), Some(7));
        assert_eq!(resolve_after_sequence(Some(5), Some("event-id")), Some(5));
        assert_eq!(resolve_after_sequence(None, Some("0")), None);
    }

    #[test]
    fn event_sse_id_uses_sequence_cursor_not_internal_event_id() {
        let mut event = EventEnvelope::domain(
            EventId::new("event-internal-id"),
            "message.created",
            json!({ "content": "hello" }),
        );
        event.sequence = 42;

        assert_eq!(event_sse_id(&event), Some("42".to_string()));

        event.sequence = 0;
        assert_eq!(event_sse_id(&event), None);
    }

    #[test]
    fn event_workspace_filter_allows_events_without_requested_workspace() {
        let state = test_state();
        let event = EventEnvelope::domain(
            EventId::new("event-sse-unscoped"),
            "message.created",
            json!({ "content": "unscoped" }),
        );

        assert!(event_matches_workspace(&state, &event, None));
    }

    #[test]
    fn event_workspace_filter_uses_explicit_event_workspace() {
        let state = test_state();
        let requested = workspace_id("workspace-a");
        let matching_event = EventEnvelope::domain(
            EventId::new("event-sse-a"),
            "workspace.changed",
            json!({ "workspace_id": "workspace-a" }),
        )
        .with_context(EventContext {
            workspace_id: Some(requested.clone()),
            ..EventContext::default()
        });
        let mismatched_event = EventEnvelope::domain(
            EventId::new("event-sse-b"),
            "workspace.changed",
            json!({ "workspace_id": "workspace-b" }),
        )
        .with_context(EventContext {
            workspace_id: Some(workspace_id("workspace-b")),
            ..EventContext::default()
        });

        assert!(event_matches_workspace(
            &state,
            &matching_event,
            Some(&requested)
        ));
        assert!(!event_matches_workspace(
            &state,
            &mismatched_event,
            Some(&requested)
        ));
    }

    #[test]
    fn event_workspace_filter_uses_payload_workspace_when_context_is_missing() {
        let state = test_state();
        let requested = workspace_id("workspace-payload-a");
        let matching_snake_event = EventEnvelope::domain(
            EventId::new("event-sse-payload-workspace-snake"),
            "session.title.updated",
            json!({
                "workspace_id": "workspace-payload-a",
                "session_id": "session-payload-a",
            }),
        );
        let matching_camel_event = EventEnvelope::domain(
            EventId::new("event-sse-payload-workspace-camel"),
            "session.title.updated",
            json!({
                "workspaceId": "workspace-payload-a",
                "sessionId": "session-payload-a",
            }),
        );
        let mismatched_event = EventEnvelope::domain(
            EventId::new("event-sse-payload-workspace-b"),
            "session.title.updated",
            json!({
                "workspace_id": "workspace-payload-b",
                "session_id": "session-payload-b",
            }),
        );

        assert!(event_matches_workspace(
            &state,
            &matching_snake_event,
            Some(&requested)
        ));
        assert!(event_matches_workspace(
            &state,
            &matching_camel_event,
            Some(&requested)
        ));
        assert!(!event_matches_workspace(
            &state,
            &mismatched_event,
            Some(&requested)
        ));
    }

    #[test]
    fn event_workspace_filter_infers_workspace_from_session_context() {
        let state = test_state();
        let requested = workspace_id("workspace-a");
        let session_a = SessionId::new("session-sse-workspace-a");
        let session_b = SessionId::new("session-sse-workspace-b");
        state
            .session_store
            .create_session_for_workspace(
                session_a.clone(),
                "workspace A",
                Some("workspace-a".to_string()),
            )
            .expect("session should create");
        state
            .session_store
            .create_session_for_workspace(
                session_b.clone(),
                "workspace B",
                Some("workspace-b".to_string()),
            )
            .expect("session should create");
        let matching_event = EventEnvelope::domain(
            EventId::new("event-sse-session-a"),
            "message.created",
            json!({ "session_id": session_a.as_str(), "content": "A" }),
        )
        .with_context(EventContext {
            session_id: Some(session_a),
            ..EventContext::default()
        });
        let mismatched_event = EventEnvelope::domain(
            EventId::new("event-sse-session-b"),
            "message.created",
            json!({ "session_id": session_b.as_str(), "content": "B" }),
        )
        .with_context(EventContext {
            session_id: Some(session_b),
            ..EventContext::default()
        });

        assert!(event_matches_workspace(
            &state,
            &matching_event,
            Some(&requested)
        ));
        assert!(!event_matches_workspace(
            &state,
            &mismatched_event,
            Some(&requested)
        ));
    }

    #[test]
    fn event_session_filter_keeps_current_session_and_workspace_global_events() {
        let session_a = SessionId::new("session-sse-a");
        let session_b = SessionId::new("session-sse-b");
        let matching_event = EventEnvelope::domain(
            EventId::new("event-sse-session-a"),
            "session.turn.item",
            json!({ "content": "A" }),
        )
        .with_context(EventContext {
            session_id: Some(session_a.clone()),
            ..EventContext::default()
        });
        let mismatched_event = EventEnvelope::domain(
            EventId::new("event-sse-session-b"),
            "session.turn.item",
            json!({ "content": "B" }),
        )
        .with_context(EventContext {
            session_id: Some(session_b),
            ..EventContext::default()
        });
        let workspace_global_event = EventEnvelope::domain(
            EventId::new("event-sse-workspace-global"),
            "workspace.changed",
            json!({ "workspace_id": "workspace-a" }),
        );

        assert!(event_matches_session(&matching_event, Some(&session_a)));
        assert!(!event_matches_session(&mismatched_event, Some(&session_a)));
        assert!(event_matches_session(
            &workspace_global_event,
            Some(&session_a)
        ));
    }

    #[test]
    fn event_session_filter_uses_payload_session_and_rejects_unscoped_task_events() {
        let session = SessionId::new("session-sse-payload");
        let payload_scoped_event = EventEnvelope::domain(
            EventId::new("event-sse-payload-session"),
            "message.created",
            json!({ "session_id": session.as_str(), "content": "payload scoped" }),
        );
        let task_event_without_session = EventEnvelope::domain(
            EventId::new("event-sse-task-unscoped"),
            "task.status.changed",
            json!({ "task_id": "task-unscoped" }),
        )
        .with_context(EventContext {
            task_id: Some(TaskId::new("task-unscoped")),
            ..EventContext::default()
        });
        let task_event_with_session = EventEnvelope::domain(
            EventId::new("event-sse-task-scoped"),
            "task.status.changed",
            json!({ "task_id": "task-scoped", "session_id": session.as_str() }),
        )
        .with_context(EventContext {
            session_id: Some(session.clone()),
            task_id: Some(TaskId::new("task-scoped")),
            ..EventContext::default()
        });

        assert!(event_matches_session(&payload_scoped_event, Some(&session)));
        assert!(event_matches_session(
            &task_event_with_session,
            Some(&session)
        ));
        assert!(!event_matches_session(
            &task_event_without_session,
            Some(&session)
        ));
    }

    #[test]
    fn event_stream_scope_resolves_workspace_from_registered_path_when_id_is_stale() {
        let state = test_state();
        let workspace_a = workspace_id("workspace-sse-path-a");
        let workspace_b = workspace_id("workspace-sse-path-b");
        state
            .workspace_registry
            .register(
                workspace_a.clone(),
                AbsolutePath::new("/tmp/magi-sse-path-a"),
            )
            .expect("workspace A should register");
        state
            .workspace_registry
            .register(
                workspace_b.clone(),
                AbsolutePath::new("/tmp/magi-sse-path-b"),
            )
            .expect("workspace B should register");

        let resolved = resolve_event_stream_workspace_id(
            &state,
            Some(workspace_b.to_string()),
            Some("/tmp/magi-sse-path-a".to_string()),
        );

        assert_eq!(resolved, Some(workspace_a));
    }

    #[test]
    fn event_stream_scope_keeps_unknown_path_restricted() {
        let state = test_state();
        let resolved = resolve_event_stream_workspace_id(
            &state,
            None,
            Some("/tmp/magi-sse-missing-path".to_string()),
        )
        .expect("unknown explicit path should still produce a restrictive scope");

        let unscoped_event = EventEnvelope::domain(
            EventId::new("event-sse-unknown-path-unscoped"),
            "message.created",
            json!({ "content": "unscoped" }),
        );

        assert!(!event_matches_workspace(
            &state,
            &unscoped_event,
            Some(&resolved)
        ));
    }

    #[test]
    fn lagged_recovery_event_is_workspace_scoped_and_bootstrap_actionable() {
        let workspace = workspace_id("workspace-lagged");
        let event = lagged_recovery_event(7, Some(&workspace));

        assert_eq!(event.event_type, "event.stream.lagged");
        assert_eq!(event.category, magi_event_bus::EventCategory::System);
        assert_eq!(event.workspace_id.as_ref(), Some(&workspace));
        assert_eq!(event.payload["skipped"], json!(7));
        assert_eq!(event.payload["recovery"], json!("bootstrap"));
    }

    #[tokio::test]
    async fn event_stream_emits_lagged_recovery_event_when_receiver_falls_behind() {
        let state = test_state_with_event_capacity(2);
        let workspace = workspace_id("workspace-lagged-stream");
        let mut events = Box::pin(event_envelope_stream(
            state.clone(),
            Some(workspace.clone()),
            None,
            None,
        ));

        for index in 0..8 {
            state
                .event_bus
                .publish(
                    EventEnvelope::domain(
                        EventId::new(format!("event-sse-lagged-{index}")),
                        "message.created",
                        json!({ "index": index }),
                    )
                    .with_context(EventContext {
                        workspace_id: Some(workspace.clone()),
                        ..EventContext::default()
                    }),
                )
                .expect("event should publish");
        }

        let event = tokio::time::timeout(Duration::from_secs(1), events.next())
            .await
            .expect("lagged recovery event should arrive")
            .expect("stream should stay open");

        assert_eq!(event.event_type, "event.stream.lagged");
        assert_eq!(event.workspace_id.as_ref(), Some(&workspace));
        assert_eq!(event.payload["reason"], json!("broadcast_lagged"));
        assert_eq!(event.payload["recovery"], json!("bootstrap"));
        assert!(
            event.payload["skipped"].as_u64().unwrap_or_default() > 0,
            "lagged event should expose skipped event count"
        );
    }

    #[tokio::test]
    async fn event_stream_emits_lagged_recovery_event_when_after_sequence_is_outside_retention() {
        let state = test_state_with_event_capacity(2);
        let workspace = workspace_id("workspace-retention-gap");
        for index in 0..5 {
            state
                .event_bus
                .publish(
                    EventEnvelope::domain(
                        EventId::new(format!("event-retention-gap-{index}")),
                        "message.created",
                        json!({ "index": index }),
                    )
                    .with_context(EventContext {
                        workspace_id: Some(workspace.clone()),
                        ..EventContext::default()
                    }),
                )
                .expect("event should publish");
        }

        let mut events = Box::pin(event_envelope_stream(
            state,
            Some(workspace.clone()),
            None,
            Some(1),
        ));
        let event = tokio::time::timeout(Duration::from_secs(1), events.next())
            .await
            .expect("retention gap recovery event should arrive")
            .expect("stream should stay open");

        assert_eq!(event.event_type, "event.stream.lagged");
        assert_eq!(event.workspace_id.as_ref(), Some(&workspace));
        assert_eq!(event.payload["reason"], json!("broadcast_lagged"));
        assert_eq!(event.payload["recovery"], json!("bootstrap"));
        assert!(
            event.payload["skipped"].as_u64().unwrap_or_default() > 0,
            "retention gap should expose skipped event count"
        );
    }

    #[tokio::test]
    async fn event_stream_filters_snapshot_by_after_sequence_cursor() {
        let state = test_state_with_event_capacity(8);
        let workspace = workspace_id("workspace-after-sequence");
        for index in 1..=3 {
            state
                .event_bus
                .publish(
                    EventEnvelope::domain(
                        EventId::new(format!("event-after-sequence-{index}")),
                        "message.created",
                        json!({ "index": index }),
                    )
                    .with_context(EventContext {
                        workspace_id: Some(workspace.clone()),
                        ..EventContext::default()
                    }),
                )
                .expect("event should publish");
        }

        let mut events = Box::pin(event_envelope_stream(state, Some(workspace), None, Some(2)));
        let event = tokio::time::timeout(Duration::from_secs(1), events.next())
            .await
            .expect("event after cursor should arrive")
            .expect("stream should stay open");

        assert_eq!(event.event_id.as_str(), "event-after-sequence-3");
        assert_eq!(event.sequence, 3);
    }
}
