use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use magi_core::UtcMillis;
use magi_session_store::{NotificationRecord, SessionRecord, TimelineEntry};
use serde::{Deserialize, Serialize};

use super::session_scope::{
    parse_session_id, require_current_session_record_in_workspace,
    require_session_record_in_workspace,
};
use crate::{errors::ApiError, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new().route("/messages", get(get_messages))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessagesQuery {
    session_id: Option<String>,
    #[serde(default, alias = "workspace_id")]
    workspace_id: Option<String>,
    limit: Option<usize>,
    before_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessagesResponseDto {
    generated_at: UtcMillis,
    current_session: Option<SessionRecord>,
    sessions: Vec<SessionRecord>,
    timeline: Vec<TimelineEntry>,
    notifications: Vec<NotificationRecord>,
    session_id: String,
    has_more_before: bool,
    before_cursor: Option<String>,
}

async fn get_messages(
    State(state): State<ApiState>,
    Query(query): Query<MessagesQuery>,
) -> Result<Json<MessagesResponseDto>, ApiError> {
    let requested_workspace_id = query
        .workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|workspace_id| !workspace_id.is_empty());
    let current_session = match query
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(id) => {
            let sid = parse_session_id(Some(id))?;
            require_session_record_in_workspace(&state, &sid, requested_workspace_id)?
        }
        None => require_current_session_record_in_workspace(&state, requested_workspace_id)?,
    };
    let session_id = current_session.session_id.clone();
    let scope_workspace_id = requested_workspace_id
        .map(str::to_string)
        .or_else(|| current_session.workspace_id.clone());

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
    let sessions = state.session_records_for_workspace(scope_workspace_id.as_deref());
    let before_cursor = page.first().map(|entry| entry.entry_id.clone());

    Ok(Json(MessagesResponseDto {
        generated_at: UtcMillis::now(),
        current_session: Some(current_session),
        sessions,
        timeline: page,
        notifications: state.session_store.notifications_for_session(&session_id),
        session_id: session_id.to_string(),
        has_more_before: start > 0,
        before_cursor,
    }))
}
