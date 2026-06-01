use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use magi_core::{UtcMillis, public_runtime_text};
use magi_session_store::{CanonicalTurn, NotificationRecord, SessionRecord, TimelineEntry};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::session_scope::{
    parse_session_id, require_registered_workspace_binding, require_session_record_in_workspace,
    require_workspace_id,
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
    #[serde(default, alias = "workspace_path")]
    workspace_path: Option<String>,
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
    let requested_workspace_id = match query
        .workspace_path
        .as_deref()
        .map(str::trim)
        .filter(|workspace_path| !workspace_path.is_empty())
    {
        Some(_) => {
            require_registered_workspace_binding(
                &state,
                query.workspace_id.as_deref(),
                query.workspace_path.as_deref(),
            )?
            .workspace_id
        }
        None => require_workspace_id(query.workspace_id.as_deref())?,
    };
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
    let canonical_turns = state
        .session_store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .map(public_canonical_turn)
        .collect();
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

fn public_canonical_turn(mut turn: CanonicalTurn) -> CanonicalTurn {
    for item in &mut turn.items {
        let Some(tool) = item.tool.as_mut() else {
            continue;
        };
        tool.arguments = tool.arguments.take().and_then(public_canonical_tool_value);
        tool.result = tool.result.take().and_then(public_canonical_tool_value);
        tool.error = public_canonical_tool_text(tool.error.take());
    }
    turn
}

fn public_canonical_tool_value(value: Value) -> Option<Value> {
    let public = public_runtime_text(&value.to_string());
    if public.is_empty() {
        return None;
    }
    serde_json::from_str(&public)
        .ok()
        .or_else(|| Some(Value::String(public)))
}

fn public_canonical_tool_text(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_string();
    if value.is_empty() {
        return None;
    }
    let public = public_runtime_text(&value);
    if public.is_empty() {
        None
    } else {
        Some(public)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_core::{
        AbsolutePath, ExecutionOwnership, SessionId, ThreadId, UtcMillis, WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::{
        SessionDurableState, SessionExecutionSidecarStatus, SessionExecutionSidecarStoreState,
        SessionRuntimeSidecar, SessionStore, TimelineEntryKind,
    };
    use magi_workspace::WorkspaceStore;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tower::ServiceExt;

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn test_state(session_store: SessionStore) -> ApiState {
        ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(session_store),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let unique = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "{}-{}-{}-{}",
            prefix,
            std::process::id(),
            UtcMillis::now().0,
            unique
        ));
        fs::create_dir_all(&dir).expect("temp dir should create");
        dir
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
    async fn messages_resolves_workspace_from_registered_path_when_query_id_is_stale() {
        let session_id = SessionId::new("session-messages-path-binding");
        let workspace_id = WorkspaceId::new("workspace-messages-path-binding");
        let root = unique_temp_dir("magi-messages-path-binding");
        let store = SessionStore::default();
        store
            .create_session_for_workspace(
                session_id.clone(),
                "路径绑定会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        store.append_timeline_entry(
            session_id.clone(),
            TimelineEntryKind::UserMessage,
            "来自路径绑定的消息",
        );
        let state = test_state(store);
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(root.to_string_lossy().as_ref()),
            )
            .expect("workspace should register");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/messages?workspaceId=workspace-stale-query&workspacePath={}&sessionId={}",
                        urlencoding::encode(root.to_string_lossy().as_ref()),
                        session_id
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json_response(response).await;
        assert_eq!(body["sessionId"], session_id.as_str());
        assert_eq!(
            body["currentSession"]["workspaceId"],
            workspace_id.as_str(),
            "messages must resolve workspace from registered workspacePath"
        );
        assert_eq!(body["sessions"][0]["workspaceId"], workspace_id.as_str());
        assert!(
            body["timeline"]
                .as_array()
                .expect("timeline should be array")
                .iter()
                .any(|entry| entry["message"] == "来自路径绑定的消息"),
            "messages response must include the timeline entry from the resolved workspace session"
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

    #[tokio::test]
    async fn messages_redacts_canonical_tool_payloads_without_mutating_store() {
        let session_id = SessionId::new("session-messages-tool-redaction");
        let store = SessionStore::default();
        store
            .create_session_for_workspace(
                session_id.clone(),
                "工具消息脱敏",
                Some("workspace-messages-tool-redaction".to_string()),
            )
            .expect("session should create");
        store
            .upsert_current_turn(
                session_id.clone(),
                magi_session_store::ActiveExecutionTurn {
                    turn_id: "turn-messages-tool-redaction".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(1),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("请读取文件".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("turn should upsert");
        store
            .upsert_current_turn_item(
                &session_id,
                magi_session_store::ActiveExecutionTurnItem {
                    item_id: "turn-item-tool-redaction".to_string(),
                    item_seq: 1,
                    kind: "tool_call_result".to_string(),
                    status: "failed".to_string(),
                    source: "worker".to_string(),
                    title: Some("读取文件".to_string()),
                    content: Some("工具卡片".to_string()),
                    task_id: None,
                    worker_id: None,
                    role_id: None,
                    tool_call_id: Some("tool-call-redaction".to_string()),
                    tool_name: Some("read_file".to_string()),
                    tool_status: Some("failed".to_string()),
                    tool_arguments: Some(
                        serde_json::json!({
                            "path": "/Users/xie/code/TEST/secret.txt",
                            "token": "sk-argument-secret"
                        })
                        .to_string(),
                    ),
                    tool_result: Some(
                        serde_json::json!({
                            "output": "read /private/tmp/magi/result with Bearer resulttoken"
                        })
                        .to_string(),
                    ),
                    tool_error: Some(
                        "failed at /var/folders/magi/cache with sk-error-secret".to_string(),
                    ),
                    request_id: None,
                    user_message_id: None,
                    placeholder_message_id: None,
                    metadata: Default::default(),
                    timeline_entry_id: None,
                    source_thread_id: ThreadId::new("thread-tool-redaction"),
                },
            )
            .expect("tool item should upsert");

        let raw_turn = store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == "turn-messages-tool-redaction")
            .expect("raw canonical turn should exist");
        let raw_tool = raw_turn.items[0]
            .tool
            .as_ref()
            .expect("raw tool should exist");
        assert!(
            raw_tool
                .arguments
                .as_ref()
                .expect("raw arguments should exist")
                .to_string()
                .contains("/Users/xie")
        );

        let state = test_state(store);
        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(
                        "/messages?workspaceId=workspace-messages-tool-redaction&sessionId=session-messages-tool-redaction",
                    )
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json_response(response).await;
        let body_text = body.to_string();
        assert!(!body_text.contains("/Users/xie"));
        assert!(!body_text.contains("/private/tmp"));
        assert!(!body_text.contains("/var/folders"));
        assert!(!body_text.contains("argument-secret"));
        assert!(!body_text.contains("resulttoken"));
        assert!(!body_text.contains("error-secret"));

        let tool = &body["canonicalTurns"][0]["items"][0]["tool"];
        assert_eq!(tool["arguments"]["path"], "[path]");
        assert_eq!(tool["arguments"]["token"], "[redacted]");
        assert!(
            tool["result"]["output"]
                .as_str()
                .expect("result output should be string")
                .contains("Bearer [redacted]")
        );
        assert!(
            tool["error"]
                .as_str()
                .expect("tool error should be string")
                .contains("sk-[redacted]")
        );
    }
}
