use axum::{
    http::{HeaderName, HeaderValue, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures_util::{Stream, StreamExt, stream};
use magi_core::{EventId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
use std::{convert::Infallible, time::Duration};
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};

use crate::state::ApiState;

pub async fn events(
    state: ApiState,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
) -> Response {
    let workspace_id = resolve_event_stream_workspace_id(&state, workspace_id, workspace_path);
    let stream = event_envelope_stream(state, workspace_id)
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

fn event_envelope_stream(
    state: ApiState,
    workspace_id: Option<WorkspaceId>,
) -> impl Stream<Item = EventEnvelope> {
    let (snapshot, receiver) = state.event_bus.snapshot_and_subscribe();
    let snapshot_state = state.clone();
    let live_state = state;
    let snapshot_workspace_id = workspace_id.clone();
    let live_workspace_id = workspace_id;
    let recent_stream = stream::iter(snapshot.recent_events.into_iter().filter(move |event| {
        event_matches_workspace(&snapshot_state, event, snapshot_workspace_id.as_ref())
    }));
    let live_stream = BroadcastStream::new(receiver).filter_map(move |event| {
        let live_state = live_state.clone();
        let live_workspace_id = live_workspace_id.clone();
        async move {
            match event {
                Ok(envelope) => {
                    event_matches_workspace(&live_state, &envelope, live_workspace_id.as_ref())
                        .then_some(envelope)
                }
                Err(BroadcastStreamRecvError::Lagged(skipped)) => {
                    Some(lagged_recovery_event(skipped, live_workspace_id.as_ref()))
                }
            }
        }
    });
    recent_stream.chain(live_stream)
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
    use magi_core::{AbsolutePath, EventId, SessionId};
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
        let response = events(test_state(), Some("workspace-a".to_string()), None).await;

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
}
