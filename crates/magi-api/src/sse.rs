use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{StreamExt, stream};
use magi_core::WorkspaceId;
use magi_event_bus::EventEnvelope;
use std::{convert::Infallible, time::Duration};
use tokio_stream::wrappers::BroadcastStream;

use crate::state::ApiState;

pub async fn events(
    state: ApiState,
    workspace_id: Option<String>,
) -> Sse<impl futures_util::stream::Stream<Item = Result<Event, Infallible>>> {
    let workspace_id = workspace_id
        .map(|workspace_id| workspace_id.trim().to_string())
        .filter(|workspace_id| !workspace_id.is_empty())
        .map(|workspace_id| WorkspaceId::new(workspace_id));
    let (snapshot, receiver) = state.event_bus.snapshot_and_subscribe();
    let snapshot_state = state.clone();
    let live_state = state;
    let snapshot_workspace_id = workspace_id.clone();
    let live_workspace_id = workspace_id;
    let recent_stream = stream::iter(
        snapshot
            .recent_events
            .into_iter()
            .filter(move |event| {
                event_matches_workspace(&snapshot_state, event, snapshot_workspace_id.as_ref())
            })
            .map(|event| Ok(event_to_sse(event))),
    );
    let live_stream = BroadcastStream::new(receiver).filter_map(move |event| {
        let live_state = live_state.clone();
        let live_workspace_id = live_workspace_id.clone();
        async move {
            match event {
                Ok(envelope) => {
                    event_matches_workspace(&live_state, &envelope, live_workspace_id.as_ref())
                        .then(|| Ok(event_to_sse(envelope)))
                }
                Err(_) => None,
            }
        }
    });
    let stream = recent_stream.chain(live_stream);

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
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
    event
        .session_id
        .as_ref()
        .and_then(|session_id| state.session_store.session(session_id))
        .is_some_and(|session| {
            session.workspace_id.as_deref() == Some(requested_workspace_id.as_str())
        })
}

fn event_to_sse(event: EventEnvelope) -> Event {
    let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
    Event::default()
        .id(event.event_id.to_string())
        .data(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{EventId, SessionId};
    use magi_event_bus::{EventContext, InMemoryEventBus};
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use serde_json::json;
    use std::sync::Arc;

    fn test_state() -> ApiState {
        ApiState::new(
            "magi-sse-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
    }

    fn workspace_id(id: &str) -> WorkspaceId {
        WorkspaceId::new(id)
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
}
