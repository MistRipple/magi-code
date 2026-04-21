use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use magi_core::SessionId;
use magi_session_store::TimelineEntryKind;
use serde::{Deserialize, Serialize};

use crate::{
    dto::SessionActionRequestDto,
    errors::ApiError,
    state::ApiState,
};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/messages", get(get_messages))
        .route("/messages/send", post(send_message))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessagesQuery {
    session_id: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageDto {
    entry_id: String,
    session_id: String,
    role: &'static str,
    content: String,
    timestamp: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessagesResponseDto {
    messages: Vec<MessageDto>,
    session_id: Option<String>,
}

async fn get_messages(
    State(state): State<ApiState>,
    Query(query): Query<MessagesQuery>,
) -> Result<Json<MessagesResponseDto>, ApiError> {
    let session_id = match query.session_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
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
    let limit = query.limit.unwrap_or(200);

    let messages: Vec<MessageDto> = timeline
        .into_iter()
        .filter_map(|entry| {
            let role = match &entry.kind {
                TimelineEntryKind::UserMessage => "user",
                TimelineEntryKind::AssistantMessage => "assistant",
                TimelineEntryKind::SystemNote => "system",
                _ => return None,
            };
            Some(MessageDto {
                entry_id: entry.entry_id,
                session_id: entry.session_id.to_string(),
                role,
                content: entry.message,
                timestamp: entry.occurred_at.0 as u64,
            })
        })
        .rev()
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    Ok(Json(MessagesResponseDto {
        messages,
        session_id: Some(session_id.to_string()),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendMessageRequest {
    text: String,
    session_id: Option<String>,
    deep_task: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SendMessageResponseDto {
    session_id: String,
    entry_id: String,
    root_task_id: Option<String>,
    response: Option<String>,
}

async fn send_message(
    State(state): State<ApiState>,
    Json(request): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponseDto>, ApiError> {
    let action_request = SessionActionRequestDto::from_web_message(
        request.text,
        request.session_id,
        request.deep_task.unwrap_or(false),
    );

    let accepted = super::execute_session_action_submission(&state, &action_request)?;

    let response = state
        .task_store()
        .and_then(|ts| ts.get_task(&accepted.action_task_id))
        .map(|task| task.output_refs.join("\n"))
        .filter(|s| !s.is_empty());

    Ok(Json(SendMessageResponseDto {
        session_id: accepted.session_id.to_string(),
        entry_id: accepted.entry_id,
        root_task_id: Some(accepted.root_task_id.to_string()),
        response,
    }))
}
