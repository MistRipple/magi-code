use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use magi_core::AbsolutePath;
use magi_event_bus::InMemoryEventBus;
use magi_governance::GovernanceService;
use magi_session_store::SessionStore;
use magi_workspace::WorkspaceStore;
use tower::ServiceExt;

fn state_with_workspace() -> (ApiState, tempfile::TempDir) {
    let state = ApiState::new(
        "magi-test",
        std::sync::Arc::new(InMemoryEventBus::new(16)),
        std::sync::Arc::new(SessionStore::default()),
        std::sync::Arc::new(WorkspaceStore::default()),
        std::sync::Arc::new(GovernanceService::default()),
    );
    let dir = tempfile::tempdir().unwrap();
    state
        .workspace_registry
        .register(
            WorkspaceId::new("ws-1"),
            AbsolutePath::new(dir.path().display().to_string()),
        )
        .unwrap();
    (state, dir)
}

async fn call(
    state: &ApiState,
    method: &str,
    path: &str,
    body: serde_json::Value,
    tunnel: bool,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("/api{path}"))
        .header("content-type", "application/json");
    if tunnel {
        builder = builder.header("cf-ray", "abc");
    }
    let response = super::super::build_router(state.clone())
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

#[tokio::test]
async fn conversation_client_directory_selection_is_local_once_and_per_chat() {
    let (state, _dir) = state_with_workspace();
    let (status, created) = call(&state,"POST","/mcp-server/tokens",serde_json::json!({"clientName":"Any Web Agent","attribution":"conversation","allowNetwork":true}),false).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["token"]["state"], "active");
    assert_eq!(created["token"]["workspaceId"], "");
    let record = state.mcp_service.list_tokens().remove(0);
    let principal = magi_mcp_server::ClientPrincipal::from(&record);
    let a = state
        .mcp_service
        .conversations
        .start(&principal, "A", "Chat A")
        .unwrap();
    let b = state
        .mcp_service
        .conversations
        .start(&principal, "B", "Chat B")
        .unwrap();
    let path = format!("/mcp-server/directory-requests/{}", a.session_id);
    let source = state
        .workspace_registry
        .project(&WorkspaceId::new("ws-1"))
        .unwrap()
        .primary_source_folder_id;
    let choice = serde_json::json!({"workspaceId":"ws-1","sourceFolderId":source});
    assert_eq!(
        call(
            &state,
            "GET",
            "/mcp-server/directory-requests",
            serde_json::Value::Null,
            true
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&state, "POST", &path, choice.clone(), true).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        state
            .mcp_service
            .conversations
            .bind(&state, &principal, &a.session_id)
            .is_err()
    );
    assert_eq!(
        call(&state, "POST", &path, choice.clone(), false).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&state, "POST", &path, choice.clone(), false).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(state.mcp_service.conversations.pending(&state).len(), 1);
    let path_b = format!("/mcp-server/directory-requests/{}", b.session_id);
    assert_eq!(
        call(&state, "POST", &path_b, choice, false).await.0,
        StatusCode::OK
    );
    let (status, listed) = call(
        &state,
        "GET",
        "/mcp-server/sessions",
        serde_json::Value::Null,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sessions = listed["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(
        sessions
            .iter()
            .all(|entry| entry["tokenId"] == record.token_id && entry["workspaceId"] == "ws-1")
    );
    assert_ne!(sessions[0]["sessionId"], sessions[1]["sessionId"]);
    assert_ne!(sessions[0]["title"], sessions[1]["title"]);
}

#[tokio::test]
async fn conversation_titles_hide_identifier_fallbacks_and_preserve_renames() {
    let (state, _dir) = state_with_workspace();
    let (_, created) = call(
        &state,
        "POST",
        "/mcp-server/tokens",
        serde_json::json!({
            "clientName": "Web Agent", "attribution": "conversation"
        }),
        false,
    )
    .await;
    assert!(created["token"]["tokenId"].is_string(), "{created}");
    let principal = magi_mcp_server::ClientPrincipal::from(&state.mcp_service.list_tokens()[0]);
    let conversation = state
        .mcp_service
        .conversations
        .start(&principal, "chat-identifier-not-a-title", "")
        .unwrap();
    let source = state
        .workspace_registry
        .project(&WorkspaceId::new("ws-1"))
        .unwrap()
        .primary_source_folder_id;
    let (status, _) = call(
        &state,
        "POST",
        &format!("/mcp-server/directory-requests/{}", conversation.session_id),
        serde_json::json!({"workspaceId": "ws-1", "sourceFolderId": source}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = magi_core::SessionId::new(&conversation.session_id);
    assert_eq!(state.session_store.session(&id).unwrap().title, "");
    let (_, listed) = call(
        &state,
        "GET",
        "/mcp-server/sessions",
        serde_json::Value::Null,
        false,
    )
    .await;
    assert!(listed["sessions"][0]["title"].is_null());

    // 已有会话的旧版自动 ID 标题也隐藏，但用户后续改名必须保留。
    state
        .session_store
        .rename_session(&id, &conversation.conversation_id)
        .unwrap();
    let (_, listed) = call(
        &state,
        "GET",
        "/mcp-server/sessions",
        serde_json::Value::Null,
        false,
    )
    .await;
    assert!(listed["sessions"][0]["title"].is_null());
    state
        .session_store
        .rename_session(&id, "修复登录问题")
        .unwrap();
    let (_, listed) = call(
        &state,
        "GET",
        "/mcp-server/sessions",
        serde_json::Value::Null,
        false,
    )
    .await;
    assert_eq!(listed["sessions"][0]["title"], "修复登录问题");
    assert_eq!(listed["sessions"][0]["clientName"], "Web Agent");
}

#[tokio::test]
async fn token_status_reports_expiry_revocation_and_workspace_availability() {
    let (state, dir) = state_with_workspace();
    let issued = state
        .mcp_service
        .issue_token(IssueTokenRequest {
            project_directories: Some(
                state
                    .workspace_registry
                    .capture_directory_binding(&WorkspaceId::new("ws-1"), None, None)
                    .unwrap(),
            ),
            client_name: "status-client".into(),
            workspace_id: "ws-1".into(),
            approval_mode: ApprovalMode::Ask,
            attribution: AttributionMode::External,
            ttl_ms: Some(10),
            network: false,
        })
        .await
        .unwrap();
    let at = issued.record.created_at_ms;
    let dto =
        |time| serde_json::to_value(TokenDto::from_record(&state, &issued.record, time)).unwrap();
    assert_eq!(dto(at)["state"], "active");
    assert_eq!(dto(at + 10)["state"], "expired");
    state
        .workspace_registry
        .deregister(&WorkspaceId::new("ws-1"))
        .unwrap();
    assert_eq!(dto(at)["state"], "workspace_unavailable");
    assert_eq!(dto(at)["workspaceAvailable"], false);
    state
        .workspace_registry
        .register(
            WorkspaceId::new("ws-1"),
            AbsolutePath::new(dir.path().display().to_string()),
        )
        .unwrap();
    assert_eq!(dto(at)["state"], "active");
    let mut revoked = issued.record.clone();
    revoked.revoked_at_ms = Some(at);
    assert_eq!(
        serde_json::to_value(TokenDto::from_record(&state, &revoked, at + 10)).unwrap()["state"],
        "revoked"
    );
}

#[tokio::test]
async fn token_lifecycle_never_leaks_hash_and_shows_secret_once() {
    let (state, _dir) = state_with_workspace();
    let (status, created) = call(
        &state,
        "POST",
        "/mcp-server/tokens",
        serde_json::json!({"clientName": "cursor", "workspaceId": "ws-1"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let secret = created["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("magi_mcp_"));
    assert_eq!(created["token"]["active"], true);
    assert!(
        created["token"]["expiresAtMs"].is_number(),
        "默认 90 天有效期"
    );

    let (_, listed) = call(
        &state,
        "GET",
        "/mcp-server/status",
        serde_json::json!({}),
        false,
    )
    .await;
    let text = listed.to_string();
    assert!(!text.contains(&secret), "状态不得回显令牌原文");
    assert!(!text.to_lowercase().contains("hash"), "状态不得包含哈希");
    assert_eq!(listed["tokens"].as_array().unwrap().len(), 1);

    let token_id = created["token"]["tokenId"].as_str().unwrap();
    let (status, after) = call(
        &state,
        "DELETE",
        &format!("/mcp-server/tokens/{token_id}"),
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(after["tokens"][0]["active"], false);

    let (status, _) = call(
        &state,
        "DELETE",
        "/mcp-server/tokens/nope",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tokens_have_no_tool_scope_and_removed_fields_are_rejected() {
    let (state, _dir) = state_with_workspace();
    let (status, created) = call(
        &state,
        "POST",
        "/mcp-server/tokens",
        serde_json::json!({"clientName": "x", "workspaceId": "ws-1"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["token"]["approvalMode"], "ask", "默认每次询问");
    assert!(
        !created["token"]
            .as_object()
            .unwrap()
            .contains_key("profile"),
        "令牌不再有工具范围字段"
    );
    // 已删除的字段不再被静默忽略：创建与编辑都明确拒绝。
    for removed in [
        serde_json::json!({"profile": "edit"}),
        serde_json::json!({"confirmHighRisk": true}),
    ] {
        let mut body = serde_json::json!({"clientName": "x", "workspaceId": "ws-1"});
        body.as_object_mut()
            .unwrap()
            .extend(removed.as_object().unwrap().clone());
        let (status, _) = call(&state, "POST", "/mcp-server/tokens", body, false).await;
        assert!(status.is_client_error(), "创建应拒绝已删除字段: {removed}");
        let token_id = created["token"]["tokenId"].as_str().unwrap();
        let (status, _) = call(
            &state,
            "PATCH",
            &format!("/mcp-server/tokens/{token_id}"),
            removed.clone(),
            false,
        )
        .await;
        assert!(status.is_client_error(), "编辑应拒绝已删除字段: {removed}");
    }
    let (status, _) = call(
        &state,
        "POST",
        "/mcp-server/tokens",
        serde_json::json!({"clientName": "x", "workspaceId": "missing"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn approval_mode_is_persisted_without_a_second_confirmation_step() {
    let (state, _dir) = state_with_workspace();
    let (status, created) = call(
        &state,
        "POST",
        "/mcp-server/tokens",
        serde_json::json!({
            "clientName": "cursor",
            "workspaceId": "ws-1",
            "approvalMode": "always"
        }),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["token"]["approvalMode"], "always");
    let token_id = created["token"]["tokenId"].as_str().unwrap();

    let (status, updated) = call(
        &state,
        "PATCH",
        &format!("/mcp-server/tokens/{token_id}"),
        serde_json::json!({"approvalMode": "read_write"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["tokens"][0]["approvalMode"], "read_write");

    let (status, updated) = call(
        &state,
        "PATCH",
        &format!("/mcp-server/tokens/{token_id}"),
        serde_json::json!({"approvalMode": "ask"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["tokens"][0]["approvalMode"], "ask");
}

async fn create(state: &ApiState, body: serde_json::Value) -> (String, String) {
    let (status, created) = call(state, "POST", "/mcp-server/tokens", body, false).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    (
        created["token"]["tokenId"].as_str().unwrap().to_string(),
        created["secret"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn a_token_secret_can_be_viewed_again_until_the_token_is_revoked() {
    let (state, _dir) = state_with_workspace();
    let (token_id, secret) = create(
        &state,
        serde_json::json!({"clientName": "cursor", "workspaceId": "ws-1"}),
    )
    .await;
    let path = format!("/mcp-server/tokens/{token_id}/secret");
    let (status, viewed) = call(&state, "GET", &path, serde_json::json!({}), false).await;
    assert_eq!(status, StatusCode::OK, "{viewed}");
    assert_eq!(viewed["secret"], secret, "任何权限档都可以重新查看");
    let (_, listed) = call(
        &state,
        "GET",
        "/mcp-server/status",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(listed["tokens"][0]["hasSecret"], true);
    assert!(!listed.to_string().contains(&secret), "列表仍不回显原文");
    assert!(
        state
            .mcp_service
            .audit()
            .recent(10, Some(&token_id))
            .iter()
            .any(|entry| entry.tool == "token.view_secret"),
        "每次查看原文都进审计"
    );

    let (status, _) = call(
        &state,
        "DELETE",
        &format!("/mcp-server/tokens/{token_id}"),
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&state, "GET", &path, serde_json::json!({}), false).await;
    assert_eq!(status, StatusCode::CONFLICT, "吊销后原文被清除");
}

#[tokio::test]
async fn editing_a_token_keeps_the_secret_and_enforces_the_same_risk_rules_as_creation() {
    let (state, _dir) = state_with_workspace();
    let (token_id, secret) = create(
        &state,
        serde_json::json!({"clientName": "cursor", "workspaceId": "ws-1"}),
    )
    .await;
    let path = format!("/mcp-server/tokens/{token_id}");
    // 改名 + 有效期不动权限档：不需要风险确认。
    let (status, body) = call(
        &state,
        "PATCH",
        &path,
        serde_json::json!({"clientName": "Cursor 主机", "ttlDays": 0}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["tokens"][0]["clientName"], "Cursor 主机");
    assert!(
        body["tokens"][0]["expiresAtMs"].is_null(),
        "ttlDays=0 不过期"
    );
    let audited = state.mcp_service.audit().recent(10, Some(&token_id));
    let update = audited
        .iter()
        .find(|entry| entry.tool == "token.update")
        .unwrap();
    let detail = update.detail.clone().unwrap();
    assert!(detail.contains("name: cursor → Cursor 主机"), "{detail}");
    assert!(!detail.contains(&secret), "审计不含原文");
    assert_eq!(
        state.mcp_service.token_secret(&token_id).as_deref(),
        Some(secret.as_str())
    );

    // 令牌没有工具范围，只编辑 MCP 授权方式。
    let (status, body) = call(
        &state,
        "PATCH",
        &path,
        serde_json::json!({"approvalMode": "always"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["tokens"][0]["approvalMode"], "always");
    let (status, _) = call(
        &state,
        "PATCH",
        &path,
        serde_json::json!({"allowNetwork": true}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 工作区不可改（字段被拒绝），未知令牌 404。
    let (status, _) = call(
        &state,
        "PATCH",
        &path,
        serde_json::json!({"workspaceId": "ws-2"}),
        false,
    )
    .await;
    assert!(status.is_client_error());
    let (status, _) = call(
        &state,
        "PATCH",
        "/mcp-server/tokens/nope",
        serde_json::json!({"clientName": "x"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rotating_replaces_the_secret_and_the_old_one_stops_authenticating() {
    let (state, _dir) = state_with_workspace();
    let (token_id, old_secret) = create(
        &state,
        serde_json::json!({"clientName": "cursor", "workspaceId": "ws-1"}),
    )
    .await;
    let (status, rotated) = call(
        &state,
        "POST",
        &format!("/mcp-server/tokens/{token_id}/rotate"),
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    let new_secret = rotated["secret"].as_str().unwrap().to_string();
    assert_ne!(new_secret, old_secret);
    assert_eq!(state.mcp_service.token_secret(&token_id), Some(new_secret));
}

#[tokio::test]
async fn snippets_are_per_client_with_the_real_secret_and_a_working_stdio_command() {
    let (state, dir) = state_with_workspace();
    let (token_id, secret) = create(
        &state,
        serde_json::json!({"clientName": "Cursor Work", "workspaceId": "ws-1"}),
    )
    .await;
    let (status, generic) = call(
        &state,
        "GET",
        "/mcp-server/config-snippets",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(generic["serverName"], "magi");
    assert_eq!(generic["secretFilled"], false);
    assert!(!generic.to_string().contains(&secret));

    let (status, snippets) = call(
        &state,
        "GET",
        &format!("/mcp-server/config-snippets?tokenId={token_id}"),
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let workspace_name = dir.path().file_name().unwrap().to_str().unwrap();
    assert_eq!(
        snippets["serverName"],
        server_name_for(workspace_name, "Cursor Work")
    );
    assert_eq!(snippets["secretFilled"], true);
    // 服务没启动时没有地址，片段正文为空；正文由纯函数构造，下面直接验证。
    let built = build_snippets(
        "magi-cursor-work".to_string(),
        &secret,
        true,
        Some("http://127.0.0.1:1/mcp".to_string()),
        Some("/tmp/magi.sock"),
        Some("https://mcp.example.com/mcp"),
    );
    let json = serde_json::to_string(&built).unwrap();
    assert!(json.contains(&format!("Bearer {secret}")));
    assert_eq!(
        built.stdio_json.as_ref().unwrap()["mcpServers"]["magi-cursor-work"]["args"][0],
        "mcp-relay"
    );
    assert!(
        built
            .claude_cli
            .unwrap()
            .starts_with("claude mcp add --transport http magi-cursor-work ")
    );
    let remote = built.remote_claude_cli.as_deref().unwrap();
    assert!(remote.contains("https://mcp.example.com/mcp"), "{remote}");
    assert!(remote.contains(&format!("Bearer {secret}")));
    assert_eq!(
        built.remote_json.as_ref().unwrap()["mcpServers"]["magi-cursor-work"]["url"],
        "https://mcp.example.com/mcp"
    );
    assert_eq!(server_name_for("", "  ///  "), "magi");
    assert_eq!(
        server_name_for("项目 A", "我的 Claude"),
        "magi-项目-a-我的-claude"
    );
    let _ = relay_command();
}

#[tokio::test]
async fn named_tunnel_routes_validate_save_and_never_echo_the_token() {
    use base64::Engine as _;
    let token = base64::engine::general_purpose::STANDARD
        .encode(r#"{"a":"acct","t":"11111111-2222-3333-4444-555555555555","s":"c2VjcmV0"}"#);
    let (state, _dir) = state_with_workspace();
    let (status, _) = call(
        &state,
        "PUT",
        "/mcp-server/named-tunnel",
        serde_json::json!({"hostname": "mcp.example.com", "token": "oops"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, saved) = call(
            &state,
            "PUT",
            "/mcp-server/named-tunnel",
            serde_json::json!({"hostname": "mcp.example.com", "token": format!("cloudflared service install {token}")}),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["network"]["mode"], "named");
    assert_eq!(saved["network"]["namedHostname"], "mcp.example.com");
    assert!(!saved.to_string().contains(&token), "接口不得回显隧道令牌");

    // 网络模式没开时，连通性检查不发请求，直接说明原因。
    let (status, verify) = call(
        &state,
        "POST",
        "/mcp-server/named-tunnel/verify",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(verify["code"], "network_off");

    let (status, cleared) = call(
        &state,
        "DELETE",
        "/mcp-server/named-tunnel",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["network"]["mode"], "quick");
    let (_, verify) = call(
        &state,
        "POST",
        "/mcp-server/named-tunnel/verify",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(verify["code"], "not_configured");
}

#[tokio::test]
async fn audit_is_paged_and_can_be_cleared_per_client_or_entirely() {
    let (state, _dir) = state_with_workspace();
    let (a, _) = create(
        &state,
        serde_json::json!({"clientName": "a", "workspaceId": "ws-1"}),
    )
    .await;
    let (b, _) = create(
        &state,
        serde_json::json!({"clientName": "b", "workspaceId": "ws-1"}),
    )
    .await;
    for (token, count) in [(&a, 5), (&b, 3)] {
        for _ in 0..count {
            let record = find_token(&state, token).unwrap();
            audit_management(&state, &record, "token.view_secret", None);
        }
    }
    let (status, page) = call(
        &state,
        "GET",
        "/mcp-server/audit?limit=3&offset=0",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 8);
    assert_eq!(page["entries"].as_array().unwrap().len(), 3);
    let (_, last) = call(
        &state,
        "GET",
        "/mcp-server/audit?limit=3&offset=6",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(last["entries"].as_array().unwrap().len(), 2);
    let (_, only_b) = call(
        &state,
        "GET",
        &format!("/mcp-server/audit?tokenId={b}"),
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(only_b["total"], 3);

    let (status, cleared) = call(
        &state,
        "DELETE",
        &format!("/mcp-server/audit?tokenId={b}"),
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["removed"], 3);
    let (_, rest) = call(
        &state,
        "GET",
        "/mcp-server/audit",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(rest["total"], 5);
    let (_, all) = call(
        &state,
        "DELETE",
        "/mcp-server/audit",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(all["removed"], 5);
    let (_, empty) = call(
        &state,
        "GET",
        "/mcp-server/audit",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(empty["total"], 0);

    // 公网来源不能读取或清理。
    for method in ["GET", "DELETE"] {
        let (status, _) = call(
            &state,
            method,
            "/mcp-server/audit",
            serde_json::json!({}),
            true,
        )
        .await;
        assert!(
            status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
            "{method}"
        );
    }
}

#[tokio::test]
async fn direct_access_requires_risk_confirmation_a_network_token_and_valid_input() {
    let (state, _dir) = state_with_workspace();
    let enable = |extra: serde_json::Value| {
        let mut body = serde_json::json!({
            "enabled": true,
            "bindHost": "127.0.0.1",
            "publicHosts": ["203.0.113.5"],
        });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    // 没确认风险。
    let (status, _) = call(
        &state,
        "PUT",
        "/mcp-server/direct",
        enable(serde_json::json!({})),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // 没有允许网络使用的令牌。
    let confirmed = serde_json::json!({"confirmRisk": true});
    let (status, body) = call(
        &state,
        "PUT",
        "/mcp-server/direct",
        enable(confirmed.clone()),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    create(
        &state,
        serde_json::json!({"clientName": "remote", "workspaceId": "ws-1", "allowNetwork": true}),
    )
    .await;
    for bad in [
        serde_json::json!({"confirmRisk": true, "publicHosts": []}),
        serde_json::json!({"confirmRisk": true, "publicHosts": ["0.0.0.0"]}),
        serde_json::json!({"confirmRisk": true, "bindHost": "example.com"}),
        serde_json::json!({"confirmRisk": true, "port": 0}),
    ] {
        let (status, body) = call(
            &state,
            "PUT",
            "/mcp-server/direct",
            enable(bad.clone()),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, ok) = call(
        &state,
        "PUT",
        "/mcp-server/direct",
        enable(confirmed),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ok}");
    assert_eq!(ok["direct"]["listening"], true);
    assert_eq!(ok["direct"]["publicHosts"][0], "203.0.113.5");

    // 公网来源不能管理；关闭不需要确认。
    let (status, _) = call(
        &state,
        "PUT",
        "/mcp-server/direct",
        serde_json::json!({"enabled": false}),
        true,
    )
    .await;
    assert!(status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN);
    let (status, off) = call(
        &state,
        "PUT",
        "/mcp-server/direct",
        serde_json::json!({"enabled": false}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(off["direct"]["enabled"], false);
}

#[test]
fn require_local_rejects_public_tunnel_requests_itself() {
    let mut headers = HeaderMap::new();
    assert!(require_local(&headers).is_ok());
    headers.insert("cf-ray", "abc".parse().unwrap());
    assert!(matches!(
        require_local(&headers),
        Err(ApiError::Forbidden(_))
    ));
}

#[tokio::test]
async fn management_is_forbidden_from_public_tunnel_requests() {
    let (state, _dir) = state_with_workspace();
    for (method, path) in [
        ("GET", "/mcp-server/status"),
        ("GET", "/mcp-server/oauth/requests"),
        ("POST", "/mcp-server/oauth/requests/not-a-request"),
        ("POST", "/mcp-server/enabled"),
        ("POST", "/mcp-server/tokens"),
        ("POST", "/mcp-server/tokens/revoke-all"),
        ("GET", "/mcp-server/config-snippets"),
    ] {
        let (status, _) = call(
            &state,
            method,
            path,
            serde_json::json!({"enabled": true, "clientName": "x", "workspaceId": "ws-1"}),
            true,
        )
        .await;
        // 公网隧道请求先被隧道鉴权拒绝（401）；即便绕过它，管理路由本身也会返回 403。
        assert!(
            status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
            "{method} {path} -> {status}"
        );
    }
}

#[tokio::test]
async fn external_approvals_are_listed_across_sessions_and_resolved_once() {
    let (state, _dir) = state_with_workspace();
    let (_, created) = call(
        &state,
        "POST",
        "/mcp-server/tokens",
        serde_json::json!({"clientName": "cursor", "workspaceId": "ws-1"}),
        false,
    )
    .await;
    let token_id = created["token"]["tokenId"].as_str().unwrap().to_string();
    let session_id = crate::mcp_service::external_session_id(&token_id);
    let waiter = match state
        .conversation_registry
        .tool_approvals()
        .request_external_with_arguments(
            magi_conversation_runtime::PendingToolApproval {
                approval_id: "approval-1".to_string(),
                session_id: session_id.clone(),
                task_id: magi_conversation_runtime::external_approval::external_approval_task_id(
                    &token_id,
                ),
                turn_id: "external:t:c".to_string(),
                tool_call_id: "c".to_string(),
                tool_name: "file_write".to_string(),
                reason: "magi.fs.write: a.txt".to_string(),
                requested_at: magi_core::UtcMillis::now(),
                agent: None,
            },
            "{}",
        )
        .unwrap()
    {
        magi_conversation_runtime::ToolApprovalRequestOutcome::Pending(waiter) => waiter,
        _ => panic!("应进入待审批"),
    };

    let (status, listed) = call(
        &state,
        "GET",
        "/mcp-server/approvals",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["approvals"][0]["approvalId"], "approval-1");
    assert_eq!(listed["approvals"][0]["clientName"], "cursor");
    assert_eq!(listed["approvals"][0]["summary"], "magi.fs.write: a.txt");

    let (status, after) = call(
        &state,
        "POST",
        "/mcp-server/approvals/resolve",
        serde_json::json!({"approvalId": "approval-1", "decision": "allow_once"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "外部审批应支持单次授权: {after}");
    assert!(after["approvals"].as_array().unwrap().is_empty());
    assert_eq!(
        waiter.decision_rx.recv().unwrap(),
        magi_conversation_runtime::ToolApprovalDecision::AllowOnce
    );
    assert!(
        crate::interaction_service::pending_approvals(&state, &session_id)
            .unwrap()
            .is_empty()
    );
    let events = state.event_bus.snapshot();
    let resolved = events
        .recent_events
        .iter()
        .find(|event| {
            event.event_type == "tool.approval.resolved"
                && event.payload["approval_id"] == "approval-1"
        })
        .expect("托盘批准必须通知会话审批投影刷新");
    assert_eq!(resolved.payload["session_id"], session_id.as_str());
    assert_eq!(resolved.payload["decision"], "allow_once");
    assert_eq!(resolved.payload["external"], true);
    assert_eq!(resolved.payload["task_id"], format!("external:{token_id}"));

    let (status, _) = call(
        &state,
        "POST",
        "/mcp-server/approvals/resolve",
        serde_json::json!({"approvalId": "approval-1", "decision": "allow_for_turn"}),
        false,
    )
    .await;
    assert!(status.is_client_error(), "外部 MCP 只接受单次批准或拒绝");

    let (status, _) = call(
        &state,
        "POST",
        "/mcp-server/approvals/resolve",
        serde_json::json!({"approvalId": "approval-1", "decision": "deny"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "已处理的审批不能再次处理");
}

#[tokio::test]
async fn audit_lists_recent_calls_newest_first_and_filters_by_token() {
    let (state, _dir) = state_with_workspace();
    let entry = |token: &str, at_ms: u64| crate::mcp_runtime::AuditEntry {
        session_id: None,
        token_id: token.to_string(),
        client_name: "cursor".to_string(),
        workspace_id: "ws-1".to_string(),
        tool: "magi.fs.read".to_string(),
        requires_approval: false,
        paths: vec!["a.txt".to_string()],
        outcome: "succeeded".to_string(),
        detail: None,
        at_ms,
    };
    state.mcp_service.audit().record(entry("t1", 1));
    state.mcp_service.audit().record(entry("t2", 2));
    state.mcp_service.audit().record(entry("t1", 3));

    let (_, all) = call(
        &state,
        "GET",
        "/mcp-server/audit",
        serde_json::json!({}),
        false,
    )
    .await;
    let times = all["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["atMs"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(times, vec![3, 2, 1]);
    let (_, only) = call(
        &state,
        "GET",
        "/mcp-server/audit?tokenId=t1&limit=1",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(only["entries"].as_array().unwrap().len(), 1);
    assert_eq!(only["entries"][0]["atMs"], 3);
}

#[tokio::test]
async fn external_sessions_list_and_clear_with_the_records_and_retention_is_validated() {
    let (state, _dir) = state_with_workspace();
    for token in ["t1", "t2"] {
        crate::mcp_service::ensure_external_session(&state, token, "cursor", "ws-1").unwrap();
    }
    let (_, sessions) = call(
        &state,
        "GET",
        "/mcp-server/sessions",
        serde_json::json!({}),
        false,
    )
    .await;
    let sessions = sessions["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["clientName"], "cursor");
    assert!(
        sessions[0]["sessionId"]
            .as_str()
            .unwrap()
            .starts_with("external-tool-")
    );
    assert_eq!(sessions[0]["running"], 0);
    assert_eq!(sessions[0]["workspaceAvailable"], true);
    state
        .workspace_registry
        .deregister(&WorkspaceId::new("ws-1"))
        .unwrap();
    let (_, unavailable) = call(
        &state,
        "GET",
        "/mcp-server/sessions",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(
        unavailable["sessions"].as_array().unwrap().len(),
        2,
        "removing a workspace preserves history"
    );
    assert_eq!(unavailable["sessions"][0]["workspaceAvailable"], false);

    // 外部会话不进入用户的会话列表，清理记录时一并清除。
    call(
        &state,
        "DELETE",
        "/mcp-server/audit?tokenId=t1",
        serde_json::json!({}),
        false,
    )
    .await;
    let (_, sessions) = call(
        &state,
        "GET",
        "/mcp-server/sessions",
        serde_json::json!({}),
        false,
    )
    .await;
    let remaining = sessions["sessions"].as_array().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0]["tokenId"], "t2");

    let (status, _) = call(
        &state,
        "PUT",
        "/mcp-server/external",
        serde_json::json!({"saveContent": false, "retentionDays": 7}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, status) = call(
        &state,
        "GET",
        "/mcp-server/status",
        serde_json::json!({}),
        false,
    )
    .await;
    assert_eq!(status["external"]["saveContent"], false);
    assert_eq!(status["external"]["retentionDays"], 7);
    let (status, _) = call(
        &state,
        "PUT",
        "/mcp-server/external",
        serde_json::json!({"saveContent": true, "retentionDays": 999999}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn network_tokens_keep_full_tool_access_and_network_mode_needs_risk_confirmation() {
    let (state, _dir) = state_with_workspace();
    // 没有允许网络访问的令牌时不能开启；开启还需要显式确认风险。
    let (status, _) = call(
        &state,
        "POST",
        "/mcp-server/network",
        serde_json::json!({"enabled": true}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "未确认风险");
    let (status, body) = call(
        &state,
        "POST",
        "/mcp-server/network",
        serde_json::json!({"enabled": true, "confirmRisk": true}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "没有网络令牌: {body}");

    let (status, created) = call(
        &state,
        "POST",
        "/mcp-server/tokens",
        serde_json::json!({"clientName": "remote", "workspaceId": "ws-1", "allowNetwork": true}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["token"]["network"], true);
    assert!(
        !created["token"]
            .as_object()
            .unwrap()
            .contains_key("profile")
    );
    let (_, local) = call(
        &state,
        "POST",
        "/mcp-server/tokens",
        serde_json::json!({"clientName": "local", "workspaceId": "ws-1"}),
        false,
    )
    .await;
    assert_eq!(local["token"]["network"], false, "默认只能在本机使用");
}
