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
    let snapshot = state.event_bus.snapshot();
    let snapshot_state = state.clone();
    let live_state = state;
    let snapshot_workspace_id = workspace_id.clone();
    let live_workspace_id = workspace_id;
    let recent_stream = stream::iter(
        snapshot
            .recent_events
            .into_iter()
            .filter(move |event| event_matches_workspace(&snapshot_state, event, snapshot_workspace_id.as_ref()))
            .map(|event| Ok(event_to_sse(event))),
    );
    let live_stream =
        BroadcastStream::new(live_state.event_bus.subscribe()).filter_map(move |event| {
            let live_state = live_state.clone();
            let live_workspace_id = live_workspace_id.clone();
            async move {
            match event {
                Ok(envelope) => event_matches_workspace(&live_state, &envelope, live_workspace_id.as_ref())
                    .then(|| Ok(event_to_sse(envelope))),
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
    event.session_id
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
