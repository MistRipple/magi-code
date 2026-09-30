use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use magi_core::UtcMillis;
use magi_session_store::{CanonicalTurn, CanonicalTurnItem, SessionRecord, TimelineEntry};
use serde::{Deserialize, Serialize};

use super::session_scope::{
    parse_session_id, require_session_record_in_scope, resolve_explicit_session_scope,
};
use crate::{
    dto::SessionScopeKindDto,
    errors::ApiError,
    public_canonical::{
        HISTORY_PAGE_BYTE_BUDGET, history_page_canonical_item, history_page_canonical_turn,
        public_canonical_turn_item, trim_history_page_to_budget,
    },
    state::ApiState,
};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/messages", get(get_messages))
        .route("/messages/item", get(get_message_item))
        .route("/messages/turn-items", get(get_turn_items))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MessagesQuery {
    scope: SessionScopeKindDto,
    session_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    limit: Option<usize>,
    before_cursor: Option<String>,
    canonical_before_cursor: Option<String>,
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
    session_id: String,
    has_more_before: bool,
    before_cursor: Option<String>,
    canonical_has_more_before: bool,
    canonical_before_cursor: Option<String>,
}

async fn get_messages(
    State(state): State<ApiState>,
    Query(query): Query<MessagesQuery>,
) -> Result<Json<MessagesResponseDto>, ApiError> {
    let scope = resolve_explicit_session_scope(
        &state,
        query.scope,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
    let sid = parse_session_id(query.session_id.as_deref())?;
    let current_session = require_session_record_in_scope(&state, &sid, &scope)?;
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
    let requested_workspace_id = scope.workspace_id();
    let sessions =
        state.session_records_for_workspace(requested_workspace_id.as_ref().map(|id| id.as_str()));
    let canonical_limit = limit.min(20);
    let (canonical_turns, canonical_has_more_before, canonical_before_cursor) = state
        .session_store
        .canonical_turn_page_for_session(
            &session_id,
            query.canonical_before_cursor.as_deref(),
            canonical_limit,
        )
        .ok_or_else(|| ApiError::InvalidInput("canonical turn 游标不存在".to_string()))?;
    let mut canonical_turns = canonical_turns
        .into_iter()
        .map(history_page_canonical_turn)
        .collect::<Vec<_>>();
    let canonical_trimmed =
        trim_history_page_to_budget(&mut canonical_turns, HISTORY_PAGE_BYTE_BUDGET);
    let canonical_has_more_before = canonical_has_more_before || canonical_trimmed;
    let canonical_before_cursor = if canonical_trimmed {
        canonical_turns.first().map(|turn| turn.turn_id.clone())
    } else {
        canonical_before_cursor
    };
    let before_cursor = page.first().map(|entry| entry.entry_id.clone());

    Ok(Json(MessagesResponseDto {
        generated_at: UtcMillis::now(),
        current_session: Some(current_session),
        sessions,
        timeline: page,
        canonical_turns,
        session_id: session_id.to_string(),
        has_more_before: start > 0 || canonical_has_more_before,
        before_cursor,
        canonical_has_more_before,
        canonical_before_cursor,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MessageItemQuery {
    scope: SessionScopeKindDto,
    session_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    turn_id: String,
    item_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageItemResponseDto {
    item: CanonicalTurnItem,
}

/// 取回单个 canonical item 的完整事实。历史分页会截断超长工具输出，用户展开时由这里补全。
async fn get_message_item(
    State(state): State<ApiState>,
    Query(query): Query<MessageItemQuery>,
) -> Result<Json<MessageItemResponseDto>, ApiError> {
    let scope = resolve_explicit_session_scope(
        &state,
        query.scope,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
    let sid = parse_session_id(query.session_id.as_deref())?;
    let session = require_session_record_in_scope(&state, &sid, &scope)?;
    let item = state
        .session_store
        .canonical_turn_for_session_turn_id(&session.session_id, &query.turn_id)
        .and_then(|turn| {
            turn.items
                .into_iter()
                .find(|item| item.item_id == query.item_id)
        })
        .ok_or_else(|| ApiError::InvalidInput("消息条目不存在".to_string()))?;
    Ok(Json(MessageItemResponseDto {
        item: public_canonical_turn_item(item),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TurnItemsQuery {
    scope: SessionScopeKindDto,
    session_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    turn_id: String,
    before_item_seq: usize,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TurnItemsResponseDto {
    turn_id: String,
    items: Vec<CanonicalTurnItem>,
    /// 更早是否还有未下发的条目；`before_item_seq` 是下一次请求的游标。
    has_more_before: bool,
    before_item_seq: Option<usize>,
    omitted_item_count: usize,
}

/// 补齐被历史窗口折叠的 turn 内更早条目。turn 仍是分页单位，这里只是回合内的向前翻页。
async fn get_turn_items(
    State(state): State<ApiState>,
    Query(query): Query<TurnItemsQuery>,
) -> Result<Json<TurnItemsResponseDto>, ApiError> {
    let scope = resolve_explicit_session_scope(
        &state,
        query.scope,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
    let sid = parse_session_id(query.session_id.as_deref())?;
    let session = require_session_record_in_scope(&state, &sid, &scope)?;
    let mut turn = state
        .session_store
        .canonical_turn_for_session_turn_id(&session.session_id, &query.turn_id)
        .ok_or_else(|| ApiError::InvalidInput("消息回合不存在".to_string()))?;
    turn.normalize();
    // 用户消息始终随窗口下发，不参与向前翻页。
    let mut older = turn
        .items
        .into_iter()
        .filter(|item| {
            item.item_seq < query.before_item_seq
                && item.kind != magi_session_store::CanonicalTurnItemKind::UserMessage
        })
        .collect::<Vec<_>>();
    let limit = query.limit.unwrap_or(150).clamp(1, 300);
    let split = older.len().saturating_sub(limit);
    let page = older.split_off(split);
    let omitted_item_count = older.len();
    Ok(Json(TurnItemsResponseDto {
        turn_id: query.turn_id,
        before_item_seq: page.first().map(|item| item.item_seq),
        has_more_before: omitted_item_count > 0,
        omitted_item_count,
        items: page.into_iter().map(history_page_canonical_item).collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_conversation_runtime::SessionTurnCoordinator;
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

    fn register_workspace(state: &ApiState, workspace_id: &str) {
        let root = unique_temp_dir(workspace_id);
        state
            .workspace_registry
            .register(
                WorkspaceId::new(workspace_id),
                AbsolutePath::new(root.to_string_lossy().as_ref()),
            )
            .expect("workspace should register");
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
    async fn messages_rejects_personal_scope_for_workspace_session() {
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
                    .uri("/messages?scope=personal&sessionId=session-messages-requires-workspace")
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
                .contains("不属于个人会话"),
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
        register_workspace(&state, "workspace-messages-required");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/messages?scope=workspace&workspaceId=workspace-messages-required")
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
                        "/messages?scope=workspace&workspaceId=workspace-stale-query&workspacePath={}&sessionId={}",
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
                    last_completed_at: None,
                    last_viewed_at: None,
                }],
                timeline: Vec::new(),
                canonical_turns: Vec::new(),
                notifications: Vec::new(),
                goals: Vec::new(),
                plans: Vec::new(),
                thread_context_checkpoints: Vec::new(),
                thread_registry: Vec::new(),
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
        )
        .expect("sidecar-only persisted state should construct for rejection test");
        let state = test_state(store);
        register_workspace(&state, "workspace-sidecar-only");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(
                        "/messages?scope=workspace&workspaceId=workspace-sidecar-only&sessionId=session-messages-sidecar-only",
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
        register_workspace(&state, "workspace-messages-pagination");

        let first = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri(
                        "/messages?scope=workspace&workspaceId=workspace-messages-pagination&sessionId=session-messages-pagination&limit=3",
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
                        "/messages?scope=workspace&workspaceId=workspace-messages-pagination&sessionId=session-messages-pagination&limit=3&beforeCursor={before_cursor}",
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
    async fn messages_paginates_canonical_turns_independently_from_timeline() {
        let session_id = SessionId::new("session-messages-canonical-pagination");
        let store = SessionStore::default();
        store
            .create_session_for_workspace(
                session_id.clone(),
                "canonical 分页会话",
                Some("workspace-messages-canonical-pagination".to_string()),
            )
            .expect("session should create");
        let coordinator = SessionTurnCoordinator::new();
        for index in 0..25 {
            crate::routes::test_turn_fixtures::seed_conversation_turn(
                &store,
                &coordinator,
                &session_id,
                &format!("turn-{index:02}"),
                index + 1,
                UtcMillis(index + 1),
                "completed",
                &format!("消息 {index}"),
            );
        }
        let state = test_state(store);
        register_workspace(&state, "workspace-messages-canonical-pagination");

        let first = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri(
                        "/messages?scope=workspace&workspaceId=workspace-messages-canonical-pagination&sessionId=session-messages-canonical-pagination",
                    )
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        let first_body = read_json_response(first).await;
        let first_cursor = first_body["canonicalBeforeCursor"]
            .as_str()
            .expect("first canonical page should expose cursor");
        assert_eq!(first_body["canonicalTurns"].as_array().unwrap().len(), 20);
        assert_eq!(first_body["canonicalHasMoreBefore"], true);

        let second = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/messages?scope=workspace&workspaceId=workspace-messages-canonical-pagination&sessionId=session-messages-canonical-pagination&canonicalBeforeCursor={first_cursor}"
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        let second_body = read_json_response(second).await;
        let first_ids = first_body["canonicalTurns"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|turn| turn["turnId"].as_str())
            .collect::<std::collections::HashSet<_>>();
        let second_ids = second_body["canonicalTurns"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|turn| turn["turnId"].as_str())
            .collect::<std::collections::HashSet<_>>();
        assert!(first_ids.is_disjoint(&second_ids));
        assert_eq!(second_ids.len(), 5);
        assert_eq!(second_body["canonicalHasMoreBefore"], false);
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
        let coordinator = SessionTurnCoordinator::new();
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &store,
            &coordinator,
            &session_id,
            "turn-messages-tool-redaction",
            1,
            UtcMillis(1),
            "running",
            "请读取文件",
        );
        magi_conversation_runtime::CanonicalTurnEventSink::for_store(&store, None)
            .upsert_item_sidecar(
                &session_id,
                Some("turn-messages-tool-redaction"),
                magi_session_store::ActiveExecutionTurnItem {
                    item_id: "turn-item-tool-redaction".to_string(),
                    item_seq: 2,
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
        let raw_tool = raw_turn
            .items
            .iter()
            .find(|item| item.kind == magi_session_store::CanonicalTurnItemKind::ToolCall)
            .expect("raw tool item should exist")
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
        register_workspace(&state, "workspace-messages-tool-redaction");
        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(
                        "/messages?scope=workspace&workspaceId=workspace-messages-tool-redaction&sessionId=session-messages-tool-redaction",
                    )
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json_response(response).await;
        let body_text = body.to_string();
        assert!(!body_text.contains("/Users/xie/code/TEST/secret.txt"));
        assert!(!body_text.contains("/private/tmp"));
        assert!(!body_text.contains("/var/folders"));
        assert!(!body_text.contains("argument-secret"));
        assert!(!body_text.contains("resulttoken"));
        assert!(!body_text.contains("error-secret"));

        let tool = &body["canonicalTurns"][0]["items"]
            .as_array()
            .expect("canonical items should be array")
            .iter()
            .find(|item| item["kind"] == "tool_call")
            .expect("public tool item should exist")["tool"];
        assert_eq!(tool["arguments"]["path"], "secret.txt");
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

    #[tokio::test]
    async fn messages_truncates_long_tool_output_and_item_endpoint_returns_full_text() {
        let session_id = SessionId::new("session-messages-long-tool");
        let store = SessionStore::default();
        store
            .create_session_for_workspace(
                session_id.clone(),
                "长工具输出",
                Some("workspace-messages-long-tool".to_string()),
            )
            .expect("session should create");
        let coordinator = SessionTurnCoordinator::new();
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &store,
            &coordinator,
            &session_id,
            "turn-long-tool",
            1,
            UtcMillis(1),
            "running",
            "请读取文件",
        );
        let long_output = "x".repeat(20_000);
        magi_conversation_runtime::CanonicalTurnEventSink::for_store(&store, None)
            .upsert_item_sidecar(
                &session_id,
                Some("turn-long-tool"),
                magi_session_store::ActiveExecutionTurnItem {
                    item_id: "turn-item-long-tool".to_string(),
                    item_seq: 2,
                    kind: "tool_call_result".to_string(),
                    status: "completed".to_string(),
                    source: "worker".to_string(),
                    title: Some("读取文件".to_string()),
                    content: Some("工具卡片".to_string()),
                    task_id: None,
                    worker_id: None,
                    role_id: None,
                    tool_call_id: Some("tool-call-long".to_string()),
                    tool_name: Some("file_read".to_string()),
                    tool_status: Some("completed".to_string()),
                    tool_arguments: Some(serde_json::json!({ "path": "a.txt" }).to_string()),
                    tool_result: Some(serde_json::json!({ "content": long_output }).to_string()),
                    tool_error: None,
                    request_id: None,
                    user_message_id: None,
                    placeholder_message_id: None,
                    metadata: Default::default(),
                    timeline_entry_id: None,
                    source_thread_id: ThreadId::new("thread-long-tool"),
                },
            )
            .expect("tool item should upsert");
        let state = test_state(store);
        register_workspace(&state, "workspace-messages-long-tool");

        let page = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/messages?scope=workspace&workspaceId=workspace-messages-long-tool&sessionId=session-messages-long-tool")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        let body = read_json_response(page).await;
        let item = body["canonicalTurns"][0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["kind"] == "tool_call")
            .expect("tool item should exist")
            .clone();
        assert_eq!(
            item["tool"]["result"]["content"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            crate::public_canonical::HISTORY_TOOL_RESULT_STRING_LIMIT
        );
        assert_eq!(
            item["metadata"]["historyCompaction"]["resultTruncated"],
            true
        );
        assert_eq!(
            item["metadata"]["historyCompaction"]["omittedChars"],
            20_000 - 2 * 1024
        );

        let full = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/messages/item?scope=workspace&workspaceId=workspace-messages-long-tool&sessionId=session-messages-long-tool&turnId=turn-long-tool&itemId=turn-item-long-tool")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(full.status(), StatusCode::OK);
        let full_body = read_json_response(full).await;
        assert_eq!(
            full_body["item"]["tool"]["result"]["content"]
                .as_str()
                .unwrap()
                .len(),
            20_000
        );
    }

    #[tokio::test]
    async fn long_turn_is_windowed_and_older_items_are_fetched_by_item_cursor() {
        let session_id = SessionId::new("session-messages-long-turn");
        let store = SessionStore::default();
        store
            .create_session_for_workspace(
                session_id.clone(),
                "超长回合",
                Some("workspace-messages-long-turn".to_string()),
            )
            .expect("session should create");
        let coordinator = SessionTurnCoordinator::new();
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &store,
            &coordinator,
            &session_id,
            "turn-long",
            1,
            UtcMillis(1),
            "running",
            "请分析项目",
        );
        let total_tools = crate::public_canonical::HISTORY_TURN_ITEM_WINDOW * 2 + 20;
        let sink = magi_conversation_runtime::CanonicalTurnEventSink::for_store(&store, None);
        for index in 0..total_tools {
            sink.upsert_item_sidecar(
                &session_id,
                Some("turn-long"),
                magi_session_store::ActiveExecutionTurnItem {
                    item_id: format!("turn-item-tool-{index:04}"),
                    item_seq: 10 + index,
                    kind: "tool_call_result".to_string(),
                    status: "completed".to_string(),
                    source: "worker".to_string(),
                    title: Some("读取文件".to_string()),
                    content: Some("工具卡片".to_string()),
                    task_id: None,
                    worker_id: None,
                    role_id: None,
                    tool_call_id: Some(format!("tool-call-{index:04}")),
                    tool_name: Some("file_read".to_string()),
                    tool_status: Some("completed".to_string()),
                    tool_arguments: Some(serde_json::json!({ "path": "a.txt" }).to_string()),
                    tool_result: Some(serde_json::json!({ "content": "ok" }).to_string()),
                    tool_error: None,
                    request_id: None,
                    user_message_id: None,
                    placeholder_message_id: None,
                    metadata: Default::default(),
                    timeline_entry_id: None,
                    source_thread_id: ThreadId::new("thread-long-turn"),
                },
            )
            .expect("tool item should upsert");
        }
        let state = test_state(store);
        register_workspace(&state, "workspace-messages-long-turn");
        let base = "scope=workspace&workspaceId=workspace-messages-long-turn&sessionId=session-messages-long-turn";

        let page = read_json_response(
            routes()
                .with_state(state.clone())
                .oneshot(
                    Request::builder()
                        .uri(format!("/messages?{base}"))
                        .body(Body::empty())
                        .expect("request should build"),
                )
                .await
                .expect("route should respond"),
        )
        .await;
        let turn = &page["canonicalTurns"][0];
        let window = crate::public_canonical::HISTORY_TURN_ITEM_WINDOW;
        let items = turn["items"].as_array().unwrap();
        assert!(items.len() <= window + 1, "窗口只带最新条目和用户消息");
        assert!(items.iter().any(|item| item["kind"] == "user_message"));
        let history_window = &turn["metadata"]["historyWindow"];
        let before_seq = history_window["beforeItemSeq"]
            .as_u64()
            .expect("window cursor") as usize;
        let omitted = history_window["omittedItemCount"]
            .as_u64()
            .expect("omitted count") as usize;
        assert!(omitted > 0);

        let mut seen = items
            .iter()
            .map(|item| item["itemId"].as_str().unwrap().to_string())
            .collect::<std::collections::HashSet<_>>();
        let mut cursor = Some(before_seq);
        let mut guard = 0;
        while let Some(before) = cursor {
            guard += 1;
            assert!(guard < 10, "向前翻页必须收敛");
            let older = read_json_response(
                routes()
                    .with_state(state.clone())
                    .oneshot(
                        Request::builder()
                            .uri(format!(
                                "/messages/turn-items?{base}&turnId=turn-long&beforeItemSeq={before}"
                            ))
                            .body(Body::empty())
                            .expect("request should build"),
                    )
                    .await
                    .expect("route should respond"),
            )
            .await;
            for item in older["items"].as_array().unwrap() {
                assert!(
                    item["itemSeq"].as_u64().unwrap() < before as u64,
                    "只返回游标之前的条目"
                );
                seen.insert(item["itemId"].as_str().unwrap().to_string());
            }
            cursor = if older["hasMoreBefore"] == true {
                older["beforeItemSeq"].as_u64().map(|seq| seq as usize)
            } else {
                None
            };
        }
        let expected = crate::public_canonical::HISTORY_TURN_ITEM_WINDOW * 2 + 20 + 1;
        assert!(
            seen.len() >= expected,
            "窗口加向前翻页必须补齐整个回合: {} < {expected}",
            seen.len()
        );
    }
}
