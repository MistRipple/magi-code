use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use magi_core::UtcMillis;
use magi_session_store::{CanonicalTurn, NotificationRecord, SessionRecord, TimelineEntry};
use serde::{Deserialize, Serialize};

use super::session_scope::{
    parse_session_id, require_session_record_in_workspace, require_workspace_id,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    canonical_turns: Vec<CanonicalTurn>,
    notifications: Vec<NotificationRecord>,
    session_id: String,
    has_more_before: bool,
    before_cursor: Option<String>,
}

async fn get_messages(
    State(state): State<ApiState>,
    Query(query): Query<MessagesQuery>,
) -> Result<Json<MessagesResponseDto>, ApiError> {
    let requested_workspace_id = require_workspace_id(query.workspace_id.as_deref())?;
    let sid = parse_session_id(query.session_id.as_deref())?;
    let current_session =
        require_session_record_in_workspace(&state, &sid, Some(requested_workspace_id.as_str()))?;
    let session_id = current_session.session_id.clone();

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
    let sessions = state.session_records_for_workspace(Some(requested_workspace_id.as_str()));
    let canonical_turns = state.session_store.canonical_turns_for_session(&session_id);
    let before_cursor = page.first().map(|entry| entry.entry_id.clone());

    Ok(Json(MessagesResponseDto {
        generated_at: UtcMillis::now(),
        current_session: Some(current_session),
        sessions,
        timeline: page,
        canonical_turns,
        notifications: state.session_store.notifications_for_session(&session_id),
        session_id: session_id.to_string(),
        has_more_before: start > 0,
        before_cursor,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_core::{ExecutionOwnership, SessionId, UtcMillis, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::{
        SessionDurableState, SessionExecutionSidecarStatus, SessionExecutionSidecarStoreState,
        SessionRuntimeSidecar, SessionStore, TimelineEntryKind,
    };
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_state(session_store: SessionStore) -> ApiState {
        ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(session_store),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
    }

    async fn read_json_response(response: axum::response::Response) -> serde_json::Value {
        serde_json::from_slice::<serde_json::Value>(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body should read"),
        )
        .expect("body should be json")
    }

    #[tokio::test]
    async fn messages_requires_workspace_scope() {
        let session_id = SessionId::new("session-messages-requires-workspace");
        let store = SessionStore::default();
        store
            .create_session_for_workspace(
                session_id.clone(),
                "必须绑定工作区查询",
                Some("workspace-messages-required".to_string()),
            )
            .expect("session should create");
        store.append_timeline_entry(
            session_id.clone(),
            TimelineEntryKind::UserMessage,
            "真实消息",
        );
        let state = test_state(store);

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/messages?sessionId=session-messages-requires-workspace")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = read_json_response(response).await;
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("workspaceId 不能为空"),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn messages_requires_explicit_session_scope() {
        let session_id = SessionId::new("session-messages-requires-session");
        let store = SessionStore::default();
        store
            .create_session_for_workspace(
                session_id.clone(),
                "必须指定会话查询",
                Some("workspace-messages-required".to_string()),
            )
            .expect("session should create");
        let state = test_state(store);

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/messages?workspaceId=workspace-messages-required")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = read_json_response(response).await;
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("sessionId 不能为空"),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn messages_reject_sidecar_only_workspace_binding() {
        let session_id = SessionId::new("session-messages-sidecar-only");
        let now = UtcMillis::now();
        let store = SessionStore::from_persisted_parts(
            SessionDurableState {
                current_session_id: Some(session_id.clone()),
                sessions: vec![SessionRecord {
                    session_id: session_id.clone(),
                    title: "仅执行侧归属".to_string(),
                    status: magi_core::SessionLifecycleStatus::Active,
                    created_at: now,
                    updated_at: now,
                    message_count: None,
                    workspace_id: None,
                }],
                timeline: Vec::new(),
                canonical_turns: Vec::new(),
                notifications: Vec::new(),
            },
            SessionExecutionSidecarStoreState {
                runtime_sidecars: vec![SessionRuntimeSidecar {
                    session_id: session_id.clone(),
                    ownership: ExecutionOwnership {
                        workspace_id: Some(WorkspaceId::new("workspace-sidecar-only")),
                        ..ExecutionOwnership::default()
                    },
                    recovery_id: None,
                    current_turn: None,
                    active_execution_chain: None,
                    status: SessionExecutionSidecarStatus::Bound,
                    updated_at: now,
                }],
            },
        );
        let state = test_state(store);

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(
                        "/messages?workspaceId=workspace-sidecar-only&sessionId=session-messages-sidecar-only",
                    )
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = read_json_response(response).await;
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("不属于 workspace workspace-sidecar-only"),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn messages_paginates_without_overlap() {
        let session_id = SessionId::new("session-messages-pagination");
        let store = SessionStore::default();
        store
            .create_session_for_workspace(
                session_id.clone(),
                "分页会话",
                Some("workspace-messages-pagination".to_string()),
            )
            .expect("session should create");
        for index in 0..6 {
            store.append_timeline_entry(
                session_id.clone(),
                TimelineEntryKind::UserMessage,
                format!("用户消息 {index}"),
            );
        }
        let state = test_state(store);

        let first = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri(
                        "/messages?workspaceId=workspace-messages-pagination&sessionId=session-messages-pagination&limit=3",
                    )
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = serde_json::from_slice::<serde_json::Value>(
            &to_bytes(first.into_body(), usize::MAX)
                .await
                .expect("body should read"),
        )
        .expect("body should be json");
        let before_cursor = first_body["beforeCursor"]
            .as_str()
            .expect("first page should expose cursor");

        let second = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/messages?workspaceId=workspace-messages-pagination&sessionId=session-messages-pagination&limit=3&beforeCursor={before_cursor}",
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(second.status(), StatusCode::OK);
        let second_body = serde_json::from_slice::<serde_json::Value>(
            &to_bytes(second.into_body(), usize::MAX)
                .await
                .expect("body should read"),
        )
        .expect("body should be json");

        let first_ids = first_body["timeline"]
            .as_array()
            .expect("timeline should be array")
            .iter()
            .filter_map(|entry| entry["entryId"].as_str())
            .collect::<std::collections::HashSet<_>>();
        let second_ids = second_body["timeline"]
            .as_array()
            .expect("timeline should be array")
            .iter()
            .filter_map(|entry| entry["entryId"].as_str())
            .collect::<std::collections::HashSet<_>>();
        assert!(
            first_ids.is_disjoint(&second_ids),
            "分页结果不应重复: first={first_ids:?}, second={second_ids:?}"
        );
    }
}
