use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{stream, StreamExt};
use magi_event_bus::EventEnvelope;
use std::{convert::Infallible, time::Duration};
use tokio_stream::wrappers::BroadcastStream;

use crate::state::ApiState;

pub async fn events(
    state: ApiState,
) -> Sse<impl futures_util::stream::Stream<Item = Result<Event, Infallible>>> {
    let snapshot = state.event_bus.snapshot();
    let recent_stream = stream::iter(
        snapshot
            .recent_events
            .into_iter()
            .map(|event| Ok(event_to_sse(event))),
    );
    let live_stream =
        BroadcastStream::new(state.event_bus.subscribe()).filter_map(|event| async move {
            match event {
                Ok(envelope) => Some(Ok(event_to_sse(envelope))),
                Err(_) => None,
            }
        });
    let stream = recent_stream.chain(live_stream);

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

fn event_to_sse(event: EventEnvelope) -> Event {
    let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
    Event::default().id(event.event_id.to_string()).data(payload)
}
