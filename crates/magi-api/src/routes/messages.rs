use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use magi_core::{SessionId, UtcMillis};
use magi_session_store::{NotificationRecord, SessionRecord, TimelineEntry, TimelineEntryKind};
use serde::{Deserialize, Serialize};

use crate::{errors::ApiError, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new().route("/messages", get(get_messages))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessagesQuery {
    session_id: Option<String>,
    limit: Option<usize>,
    before_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageDto {
    role: String,
    content: String,
    entry_id: String,
    occurred_at: UtcMillis,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessagesResponseDto {
    generated_at: UtcMillis,
    current_session: Option<SessionRecord>,
    sessions: Vec<SessionRecord>,
    messages: Vec<MessageDto>,
    timeline: Vec<TimelineEntry>,
    notifications: Vec<NotificationRecord>,
    session_id: String,
    has_more_before: bool,
    before_cursor: Option<String>,
}

fn timeline_to_messages(entries: &[TimelineEntry]) -> Vec<MessageDto> {
    entries
        .iter()
        .filter_map(|entry| {
            let role = match entry.kind {
                TimelineEntryKind::UserMessage => "user",
                TimelineEntryKind::AssistantMessage => "assistant",
                _ => return None,
            };
            Some(MessageDto {
                role: role.to_string(),
                content: entry.message.clone(),
                entry_id: entry.entry_id.clone(),
                occurred_at: entry.occurred_at,
            })
        })
        .collect()
}

async fn get_messages(
    State(state): State<ApiState>,
    Query(query): Query<MessagesQuery>,
) -> Result<Json<MessagesResponseDto>, ApiError> {
    let session_id = match query
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(id) => {
            let sid = SessionId::new(id);
            if state.session_store.session(&sid).is_none() {
                return Err(ApiError::session_not_found(id));
            }
            sid
        }
        None => state
            .session_store
            .current_session()
            .map(|s| s.session_id)
            .ok_or_else(|| ApiError::InvalidInput("当前没有活动 session".to_string()))?,
    };

    let timeline = state.session_store.timeline_for_session(&session_id);
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let end = match query
        .before_cursor
        .as_deref()
        .map(str::trim)
        .filter(|cursor| !cursor.is_empty())
    {
        Some(cursor) => timeline
            .iter()
            .position(|entry| entry.entry_id == cursor)
            .ok_or_else(|| ApiError::InvalidInput("消息游标不存在".to_string()))?,
        None => timeline.len(),
    };
    let start = end.saturating_sub(limit);
    let page = timeline[start..end].to_vec();
    let current_session = state.session_store.session(&session_id);
    let before_cursor = page.first().map(|entry| entry.entry_id.clone());

    let messages = timeline_to_messages(&page);

    Ok(Json(MessagesResponseDto {
        generated_at: UtcMillis::now(),
        current_session,
        sessions: state.session_store.sessions(),
        messages,
        timeline: page,
        notifications: state.session_store.notifications_for_session(&session_id),
        session_id: session_id.to_string(),
        has_more_before: start > 0,
        before_cursor,
    }))
}
