use super::*;
use crate::scope_binding::SCOPE_BINDING_FIELDS;
use crate::state::RuntimeStatePersistence;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use magi_event_bus::InMemoryEventBus;
use magi_governance::GovernanceService;
use magi_session_store::SessionStore;
use magi_workspace::WorkspaceStore;
use std::sync::Arc;
use tower::ServiceExt;

fn test_state() -> ApiState {
    ApiState::new(
        "magi-mcp-skill-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
}

fn test_state_with_persistence(root: &Path) -> ApiState {
    test_state().with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
        root,
        root.join("workspaces.json"),
        root.join("knowledge.json"),
    )))
}

#[test]
fn skill_directory_hash_tracks_nested_content_changes() {
    let dir = tempfile::tempdir().expect("temp skill dir should create");
    std::fs::create_dir_all(dir.path().join("scripts")).expect("nested directory should create");
    std::fs::write(dir.path().join("SKILL.md"), "# demo\n").expect("skill markdown should write");
    std::fs::write(dir.path().join("scripts/run.sh"), "echo one\n")
        .expect("nested script should write");

    let first =
        read_skill_directory_metadata(dir.path()).expect("skill metadata should be readable");
    let repeated =
        read_skill_directory_metadata(dir.path()).expect("skill metadata should be stable");
    assert_eq!(first.content_hash, repeated.content_hash);
    assert_eq!(first.file_count, 2);

    std::fs::write(dir.path().join("scripts/run.sh"), "echo two\n")
        .expect("nested script should update");
    let changed = read_skill_directory_metadata(dir.path())
        .expect("updated skill metadata should be readable");
    assert_ne!(first.content_hash, changed.content_hash);
}

#[test]
fn local_skill_request_accepts_host_path_ref() {
    let dir = tempfile::tempdir().expect("temp skill dir should create");
    let path_ref = magi_core::HostPath::from_path(dir.path().to_path_buf())
        .to_path_ref()
        .as_str()
        .to_string();

    let resolved = normalize_local_instruction_skill_request_path(
        &serde_json::json!({ "directoryPath": path_ref }),
    )
    .expect("local skill path ref should resolve");

    assert_eq!(resolved, dir.path());
}

#[test]
fn github_repository_url_normalizes_to_stable_identity() {
    let repository = parse_github_repository_url("https://github.com/stellarlinkco/myclaude.git/")
        .expect("GitHub repository URL should parse");

    assert_eq!(repository.owner, "stellarlinkco");
    assert_eq!(repository.name, "myclaude");
    assert_eq!(
        repository.canonical_url,
        "https://github.com/stellarlinkco/myclaude"
    );
}

#[test]
fn repository_skill_scan_uses_skill_directory_and_frontmatter_metadata() {
    let repository_root = tempfile::tempdir().expect("repository root should create");
    let skill_dir = repository_root.path().join("skills").join("omo");
    std::fs::create_dir_all(&skill_dir).expect("skill directory should create");
    std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: omo\ndescription: 多代理编排\nauthor: Magi\nversion: 2.1.0\ncategory: orchestration\n---\n\n# OmO\n",
        )
        .expect("skill markdown should write");

    let repository = parse_github_repository_url("https://github.com/stellarlinkco/myclaude")
        .expect("GitHub repository URL should parse");
    let skills = scan_cached_repository_skills(repository_root.path(), &repository)
        .expect("repository skills should scan");

    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].skill_id, "stellarlinkco/myclaude/skills/omo");
    assert_eq!(skills[0].name, "omo");
    assert_eq!(skills[0].description, "多代理编排");
    assert_eq!(skills[0].author.as_deref(), Some("Magi"));
    assert_eq!(skills[0].version.as_deref(), Some("2.1.0"));
    assert_eq!(skills[0].category.as_deref(), Some("orchestration"));
    assert!(!skills[0].content_hash.is_empty());
    assert_eq!(skills[0].relative_path, PathBuf::from("skills/omo"));
}

#[test]
fn repository_skill_copy_preserves_complete_skill_directory() {
    let source_root = tempfile::tempdir().expect("source root should create");
    let target_root = tempfile::tempdir().expect("target root should create");
    let source = source_root.path().join("skill");
    let target = target_root.path().join("installed");
    std::fs::create_dir_all(source.join("scripts")).expect("source tree should create");
    std::fs::write(source.join("SKILL.md"), "# demo\n").expect("skill should write");
    std::fs::write(source.join("scripts").join("run.sh"), "echo ok\n")
        .expect("script should write");

    copy_repository_skill_directory(&source, &target)
        .expect("complete skill directory should copy");

    assert_eq!(
        std::fs::read_to_string(target.join("SKILL.md")).expect("skill should read"),
        "# demo\n"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("scripts").join("run.sh")).expect("script should read"),
        "echo ok\n"
    );
}

#[tokio::test]
async fn repository_skill_library_and_install_share_cached_repository_source() {
    let state_root = tempfile::tempdir().expect("state root should create");
    let repository_root = state_root
        .path()
        .join("skill_repositories")
        .join("stellarlinkco__myclaude");
    let skill_dir = repository_root.join("skills").join("omo");
    std::fs::create_dir_all(repository_root.join(".git")).expect("repository marker should create");
    std::fs::create_dir_all(skill_dir.join("scripts")).expect("skill tree should create");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: omo\ndescription: 多代理编排\n---\n\n# OmO\n",
    )
    .expect("skill markdown should write");
    std::fs::write(skill_dir.join("scripts").join("run.sh"), "echo ok\n")
        .expect("skill script should write");

    let state = test_state_with_persistence(state_root.path());
    state
        .settings_store
        .set_section(
            "repositories",
            serde_json::json!([{
                "repositoryId": "https://github.com/stellarlinkco/myclaude",
                "url": "https://github.com/stellarlinkco/myclaude",
                "lastCheckedAt": epoch_ms_now()
            }]),
        )
        .unwrap();

    let library = get_json(
        Router::new().merge(routes()).with_state(state.clone()),
        "/settings/skills/library",
    )
    .await;
    assert_eq!(library["failedRepositoryCount"], serde_json::json!(0));
    assert_eq!(library["skills"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        library["skills"][0]["skillId"],
        serde_json::json!("stellarlinkco/myclaude/skills/omo")
    );
    assert_eq!(library["skills"][0]["name"], serde_json::json!("omo"));

    let installed = post_json(
        Router::new().merge(routes()).with_state(state.clone()),
        "/settings/skills/install",
        serde_json::json!({
            "skillId": "stellarlinkco/myclaude/skills/omo"
        }),
    )
    .await;
    assert_eq!(installed["installed"], serde_json::json!(true));

    let stored = state.settings_store.get_section("skillsConfig");
    let skill = &stored["instructionSkills"][0];
    assert_eq!(
        skill["skillId"],
        serde_json::json!("stellarlinkco/myclaude/skills/omo")
    );
    assert_eq!(skill["name"], serde_json::json!("omo"));
    assert_eq!(skill["description"], serde_json::json!("多代理编排"));
    assert_eq!(skill["source"], serde_json::json!("repository"));
    let installed_dir = PathBuf::from(
        skill["directoryPath"]
            .as_str()
            .expect("installed directory should exist"),
    );
    assert_eq!(
        std::fs::read_to_string(installed_dir.join("scripts").join("run.sh"))
            .expect("installed script should read"),
        "echo ok\n"
    );

    let installed_library = get_json(
        Router::new().merge(routes()).with_state(state),
        "/settings/skills/library",
    )
    .await;
    assert_eq!(
        installed_library["skills"][0]["repositoryName"],
        serde_json::json!("https://github.com/stellarlinkco/myclaude")
    );
    assert_eq!(
        installed_library["skills"][0]["updateStatus"],
        serde_json::json!("up_to_date")
    );
}

#[tokio::test]
async fn local_skill_update_reloads_changed_content_hash() {
    let dir = tempfile::tempdir().expect("temp skill dir should create");
    std::fs::write(dir.path().join("SKILL.md"), "# demo\n\nfirst\n")
        .expect("skill markdown should write");
    let entry =
        build_local_instruction_skill_entry(dir.path()).expect("local skill entry should build");
    let original_hash = entry["loadedContentHash"]
        .as_str()
        .expect("loaded hash should exist")
        .to_string();
    std::fs::write(dir.path().join("SKILL.md"), "# demo\n\nsecond\n")
        .expect("skill markdown should update");

    let state = test_state();
    state
        .settings_store
        .set_section(
            "skillsConfig",
            serde_json::json!({ "instructionSkills": [entry] }),
        )
        .unwrap();
    let response = post_json(
        Router::new().merge(routes()).with_state(state.clone()),
        "/settings/skills/update",
        serde_json::json!({ "skillId": dir.path().file_name().unwrap().to_string_lossy() }),
    )
    .await;
    assert_eq!(response["action"], serde_json::json!("reloaded"));

    let stored = state.settings_store.get_section("skillsConfig");
    let loaded_hash = stored["instructionSkills"][0]["loadedContentHash"]
        .as_str()
        .expect("updated loaded hash should exist");
    assert_ne!(loaded_hash, original_hash);
    let current =
        read_skill_directory_metadata(dir.path()).expect("current skill metadata should read");
    assert_eq!(loaded_hash, current.content_hash);
}

#[test]
fn repository_skill_update_keeps_previous_version_for_rollback() {
    let state_root = tempfile::tempdir().expect("state root should create");
    let state = test_state_with_persistence(state_root.path());
    let skill_id = "owner/repo/skills/demo";
    let target = installed_skill_cache_root(&state)
        .unwrap()
        .join(installed_skill_cache_key(skill_id).unwrap());
    let source = state_root.path().join("repository/skills/demo");
    std::fs::create_dir_all(&target).expect("target should create");
    std::fs::create_dir_all(&source).expect("source should create");
    std::fs::write(target.join("SKILL.md"), "# demo\n\nversion one\n")
        .expect("old skill should write");
    std::fs::write(source.join("SKILL.md"), "# demo\n\nversion two\n")
        .expect("new skill should write");
    let available_metadata = read_skill_directory_metadata(&source).unwrap();
    let available = AvailableRepositorySkill {
        skill: RepositorySkill {
            skill_id: skill_id.to_string(),
            name: "demo".to_string(),
            description: "demo".to_string(),
            author: None,
            version: Some("2.0.0".to_string()),
            category: None,
            content_hash: available_metadata.content_hash,
            relative_path: PathBuf::from("skills/demo"),
        },
        repository_root: state_root.path().join("repository"),
        repository_id: "repo".to_string(),
        repository_url: "https://github.com/owner/repo".to_string(),
        commit: Some("new-commit".to_string()),
        checked_at: epoch_ms_now(),
    };
    let installed = serde_json::json!({
        "skillId": skill_id,
        "source": "repository",
        "directoryPath": target.to_string_lossy(),
        "installedContentHash": read_skill_directory_metadata(&target).unwrap().content_hash,
        "installedCommit": "old-commit",
        "version": "1.0.0",
    });

    let (_, changed) = apply_repository_skill_update(&state, &installed, &available)
        .expect("repository skill should update");
    assert!(changed);
    assert!(
        std::fs::read_to_string(target.join("SKILL.md"))
            .unwrap()
            .contains("version two")
    );
    let backup = installed_skill_backup_root(&state)
        .unwrap()
        .join(installed_skill_cache_key(skill_id).unwrap());
    assert!(
        std::fs::read_to_string(backup.join("SKILL.md"))
            .unwrap()
            .contains("version one")
    );

    rollback_repository_skill_directory(&target, &backup)
        .expect("repository skill should roll back");
    assert!(
        std::fs::read_to_string(target.join("SKILL.md"))
            .unwrap()
            .contains("version one")
    );
}

async fn post_json(app: Router, path: &str, payload: serde_json::Value) -> serde_json::Value {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .expect("request should build"),
        )
        .await
        .expect("router should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should read");
    serde_json::from_slice(&bytes).expect("response should be json")
}

async fn post_json_with_status(
    app: Router,
    path: &str,
    payload: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .expect("request should build"),
        )
        .await
        .expect("router should respond");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should read");
    let body = serde_json::from_slice(&bytes).expect("response should be json");
    (status, body)
}

async fn get_json(app: Router, path: &str) -> serde_json::Value {
    let response = app
        .oneshot(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should read");
    serde_json::from_slice(&bytes).expect("response should be json")
}

#[tokio::test]
async fn mcp_server_list_exposes_env_values_for_editing() {
    let state = test_state();
    state
        .settings_store
        .upsert_array_entry(
            "mcpServers",
            "id",
            &serde_json::json!({
                "id": "server-redacted",
                "name": "server-redacted",
                "command": "node",
                "enabled": false,
                "env": {
                    "TOKEN": "secret-token"
                }
            }),
        )
        .unwrap();
    let app = Router::new().merge(routes()).with_state(state);

    let body = get_json(app, "/settings/mcp").await;

    assert_eq!(
        body["servers"][0]["env"]["TOKEN"],
        serde_json::json!("secret-token")
    );
}

#[tokio::test]
async fn mcp_server_update_replaces_explicit_env_values() {
    let state = test_state();
    state
        .settings_store
        .upsert_array_entry(
            "mcpServers",
            "id",
            &serde_json::json!({
                "id": "server-preserve-env",
                "name": "server-preserve-env",
                "command": "node",
                "env": {
                    "TOKEN": "secret-token",
                    "MODE": "old"
                }
            }),
        )
        .unwrap();
    let app = Router::new().merge(routes()).with_state(state.clone());

    let body = post_json(
        app,
        "/settings/mcp/update",
        serde_json::json!({
            "id": "server-preserve-env",
            "name": "server-preserve-env",
            "type": "stdio",
            "command": "node",
            "env": {
                "TOKEN": "new-secret-token",
                "MODE": "new"
            },
            "enabled": false
        }),
    )
    .await;

    assert_eq!(body["updated"], true);
    let stored = stored_mcp_server_entry(&state, "server-preserve-env")
        .expect("updated server should remain stored");
    assert_eq!(
        stored["env"]["TOKEN"],
        serde_json::json!("new-secret-token")
    );
    assert_eq!(stored["env"]["MODE"], serde_json::json!("new"));
    assert_eq!(stored["enabled"], serde_json::json!(false));
}

#[tokio::test]
async fn mcp_http_server_save_list_and_update_keep_header_values_visible() {
    let state = test_state();
    let app = Router::new().merge(routes()).with_state(state.clone());
    let added = post_json(
        app,
        "/settings/mcp/add",
        serde_json::json!({
            "id": "http-server",
            "name": "http-server",
            "type": "streamable-http",
            "url": "https://example.test/mcp",
            "headers": {
                "Authorization": "Bearer secret"
            },
            "enabled": false
        }),
    )
    .await;
    assert_eq!(added["added"], true);

    let public = get_json(
        Router::new().merge(routes()).with_state(state.clone()),
        "/settings/mcp",
    )
    .await;
    assert_eq!(
        public["servers"][0]["type"],
        serde_json::json!("streamable-http")
    );
    assert_eq!(
        public["servers"][0]["headers"]["Authorization"],
        serde_json::json!("Bearer secret")
    );
    state
        .mcp_connections()
        .write()
        .expect("mcp connections should lock")
        .insert(
            "http-server".to_string(),
            Arc::new(McpServerClient::from_stdio(
                magi_bridge_client::McpServerConfig::new("unused", Vec::new()),
            )),
        );

    let updated = post_json(
        Router::new().merge(routes()).with_state(state.clone()),
        "/settings/mcp/update",
        serde_json::json!({
            "id": "http-server",
            "name": "http-server",
            "type": "streamable-http",
            "url": "https://example.test/mcp-v2",
            "headers": {
                "Authorization": "Bearer replacement"
            },
            "enabled": false
        }),
    )
    .await;
    assert_eq!(updated["updated"], true);
    assert!(
        !state
            .mcp_connections()
            .read()
            .expect("mcp connections should lock")
            .contains_key("http-server"),
        "updating MCP connection settings must invalidate the previous client"
    );
    let stored = stored_mcp_server_entry(&state, "http-server")
        .expect("HTTP MCP server should remain stored");
    assert_eq!(
        stored["url"],
        serde_json::json!("https://example.test/mcp-v2")
    );
    assert_eq!(
        stored["headers"]["Authorization"],
        serde_json::json!("Bearer replacement")
    );
    assert!(stored.get("command").is_none());
}

#[tokio::test]
async fn mcp_tools_request_connects_enabled_server_on_demand() {
    let state = test_state();
    let script = r#"
while IFS= read -r line; do
    method=$(echo "$line" | grep -o '"method":"[^"]*"' | head -1 | cut -d'"' -f4)
    id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
    case "$method" in
        initialize)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"mock-mcp","version":"1.0.0"}}}\n' "$id"
            ;;
        notifications/initialized)
            ;;
        tools/list)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"search.web","description":"Search the web"}]}}\n' "$id"
            ;;
    esac
done
"#;
    state
        .settings_store
        .upsert_array_entry(
            "mcpServers",
            "id",
            &serde_json::json!({
                "id": "auto-connect",
                "name": "auto-connect",
                "type": "stdio",
                "command": "sh",
                "args": ["-c", script],
                "enabled": true
            }),
        )
        .unwrap();
    let app = Router::new().merge(routes()).with_state(state.clone());

    let body = post_json(
        app,
        "/settings/mcp/tools",
        serde_json::json!({ "serverId": "auto-connect" }),
    )
    .await;

    assert_eq!(body["connected"], true, "unexpected body: {body}");
    assert_eq!(body["tools"][0]["name"], serde_json::json!("search.web"));
    assert!(
        state
            .mcp_connections()
            .read()
            .expect("mcp connections should lock")
            .contains_key("auto-connect")
    );
}

#[tokio::test]
async fn mcp_server_save_rejects_fields_outside_schema() {
    for (path, request, field) in [
        (
            "/settings/mcp/add",
            serde_json::json!({
                "id": "server-scope",
                "name": "server-scope",
                "type": "stdio",
                "command": "node",
                "workspaceId": "workspace-old"
            }),
            "workspaceId",
        ),
        (
            "/settings/mcp/update",
            serde_json::json!({ "server": { "id": "wrapped-server", "command": "node" } }),
            "server",
        ),
    ] {
        let app = Router::new().merge(routes()).with_state(test_state());

        let (status, body) = post_json_with_status(app, path, request).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"].as_str().unwrap_or_default().contains(field),
            "unexpected body: {body}"
        );
    }
}

#[tokio::test]
async fn mcp_server_list_drops_entries_without_id() {
    let state = test_state();
    state
        .settings_store
        .set_section(
            "mcpServers",
            serde_json::json!([
                {
                    "server": {
                        "id": "wrapped-server",
                        "command": "node"
                    }
                },
                {
                    "id": "canonical-server",
                    "command": "node",
                    "enabled": false
                }
            ]),
        )
        .unwrap();
    let app = Router::new().merge(routes()).with_state(state);

    let body = get_json(app, "/settings/mcp").await;
    let servers = body["servers"].as_array().expect("servers should be array");

    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0]["id"], serde_json::json!("canonical-server"));
}

fn assert_scope_fields_absent(value: &serde_json::Value) {
    for key in SCOPE_BINDING_FIELDS {
        assert!(
            value.get(key).is_none(),
            "global settings must not persist scope field {key}"
        );
    }
}

#[tokio::test]
async fn repository_save_strips_workspace_session_scope_fields() {
    let state = test_state();
    let app = Router::new().merge(routes()).with_state(state.clone());

    let body = post_json(
        app,
        "/settings/repositories/add",
        serde_json::json!({
            "url": "https://github.com/example/skills",
            "workspaceId": "workspace-old",
            "workspacePath": "/tmp/old",
            "sessionId": "session-old"
        }),
    )
    .await;

    assert_eq!(body["added"], true);
    let stored = state.settings_store.get_section("repositories");
    assert_scope_fields_absent(&stored[0]);
}

#[tokio::test]
async fn repository_add_requires_top_level_url() {
    for payload in [
        serde_json::json!({
            "repository": {
                "url": "https://github.com/example/skills"
            }
        }),
        serde_json::json!({
            "updates": {
                "url": "https://github.com/example/skills"
            }
        }),
    ] {
        let app = Router::new().merge(routes()).with_state(test_state());

        let (status, body) =
            post_json_with_status(app, "/settings/repositories/add", payload).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        let message = body["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("repositoryId 或 url 不能为空"),
            "unexpected body: {body}"
        );
    }
}

#[tokio::test]
async fn repository_update_accepts_top_level_id_and_updates_patch() {
    let state = test_state();
    let app = Router::new().merge(routes()).with_state(state.clone());

    let body = post_json(
        app,
        "/settings/repositories/update",
        serde_json::json!({
            "repositoryId": "https://github.com/example/skills",
            "updates": {
                "url": "https://github.com/example/skills-renamed"
            }
        }),
    )
    .await;

    assert_eq!(body["updated"], true);
    let stored = state.settings_store.get_section("repositories");
    assert_eq!(
        stored[0]["repositoryId"],
        serde_json::json!("https://github.com/example/skills")
    );
    assert_eq!(
        stored[0]["url"],
        serde_json::json!("https://github.com/example/skills-renamed")
    );
    assert!(stored[0].get("updates").is_none());
}

#[tokio::test]
async fn repository_list_cleans_response_without_mutating_persisted_settings() {
    let state = test_state();
    state
        .settings_store
        .set_section(
            "repositories",
            serde_json::json!([
                {
                    "repositoryId": "legacy-repo",
                    "url": "https://github.com/example/skills",
                    "workspaceId": "workspace-old",
                    "workspacePath": "/tmp/old",
                    "sessionId": "session-old"
                }
            ]),
        )
        .unwrap();
    let app = Router::new().merge(routes()).with_state(state.clone());

    let body = get_json(app, "/settings/repositories").await;

    assert_scope_fields_absent(&body["repositories"][0]);
    let stored = state.settings_store.get_section("repositories");
    assert_eq!(stored[0]["workspaceId"], serde_json::json!("workspace-old"));
    assert_eq!(stored[0]["sessionId"], serde_json::json!("session-old"));
}

#[test]
fn instruction_skill_normalization_strips_workspace_session_scope_fields() {
    let normalized = normalize_instruction_skill_entry(&serde_json::json!({
        "skillId": "example/skill",
        "workspaceId": "workspace-old",
        "workspacePath": "/tmp/old",
        "sessionId": "session-old"
    }))
    .expect("skill should normalize");

    assert_scope_fields_absent(&normalized);
    assert_eq!(normalized["skillId"], serde_json::json!("example/skill"));
    assert_eq!(normalized["name"], serde_json::json!("example/skill"));
    assert!(normalized.get("skillName").is_none());
}

#[tokio::test]
async fn install_skill_rejects_wrapped_requests() {
    for wrapper in ["skill", "updates"] {
        let app = Router::new().merge(routes()).with_state(test_state());

        let (status, body) = post_json_with_status(
            app,
            "/settings/skills/install",
            serde_json::json!({
                wrapper: {
                    "skillId": "example/skill"
                }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("不能包裹在 skill/updates 中"),
            "unexpected body: {body}"
        );
    }
}

#[tokio::test]
async fn skills_config_save_strips_workspace_session_scope_fields() {
    let state = test_state();
    let app = Router::new().merge(routes()).with_state(state.clone());

    let body = post_json(
        app,
        "/settings/skills/config/save",
        serde_json::json!({
            "workspaceId": "workspace-old",
            "workspacePath": "/tmp/old",
            "sessionId": "session-old",
            "instructionSkills": [
                {
                    "skillId": "example/skill",
                    "workspaceId": "workspace-old",
                    "sessionId": "session-old"
                },
                {
                    "name": "skill-without-id"
                }
            ],
            "customTools": [
                {
                    "name": "example-tool",
                    "workspacePath": "/tmp/old",
                    "sessionId": "session-old"
                }
            ]
        }),
    )
    .await;

    assert_eq!(body["saved"], true);
    let stored = state.settings_store.get_section("skillsConfig");
    assert_scope_fields_absent(&stored);
    assert_scope_fields_absent(&stored["instructionSkills"][0]);
    assert_scope_fields_absent(&stored["customTools"][0]);
    assert_eq!(
        stored["instructionSkills"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        stored["instructionSkills"][0]["skillId"],
        serde_json::json!("example/skill")
    );
}

#[tokio::test]
async fn builtin_skill_can_be_disabled_but_never_removed() {
    let root = tempfile::tempdir().expect("state root should create");
    let state = test_state();
    crate::builtin_skills::install_builtin_skills(&state.settings_store, root.path())
        .expect("builtin skills should install");
    let id = "magi-cloudflare-tunnel";

    let (status, body) = post_json_with_status(
        Router::new().merge(routes()).with_state(state.clone()),
        "/settings/skills/remove",
        serde_json::json!({ "skillId": id, "source": "instruction" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let stored = state.settings_store.get_section("skillsConfig");
    assert_eq!(stored["instructionSkills"][0]["skillId"], id);

    // 整体保存配置漏掉内置条目时，会被补回。
    post_json(
        Router::new().merge(routes()).with_state(state.clone()),
        "/settings/skills/config/save",
        serde_json::json!({ "instructionSkills": [] }),
    )
    .await;
    let stored = state.settings_store.get_section("skillsConfig");
    assert_eq!(stored["instructionSkills"][0]["skillId"], id);

    // 停用是允许的，库列表带有 builtin 标记。
    let disabled = post_json(
        Router::new().merge(routes()).with_state(state.clone()),
        "/settings/skills/toggle",
        serde_json::json!({ "skillId": id, "enabled": false }),
    )
    .await;
    assert_eq!(disabled["enabled"], serde_json::json!(false));
    let library = get_json(
        Router::new().merge(routes()).with_state(state.clone()),
        "/settings/skills/library",
    )
    .await;
    assert_eq!(library["skills"][0]["builtin"], serde_json::json!(true));
}

#[tokio::test]
async fn installed_skill_can_be_disabled_and_reenabled() {
    let skill_dir = tempfile::tempdir().expect("skill directory should create");
    std::fs::write(skill_dir.path().join("SKILL.md"), "# Toggle Skill\n")
        .expect("skill markdown should write");
    let state = test_state();
    state
        .settings_store
        .set_section(
            "skillsConfig",
            serde_json::json!({
                "instructionSkills": [{
                    "skillId": "toggle-skill",
                    "name": "Toggle Skill",
                    "directoryPath": skill_dir.path().to_string_lossy().to_string(),
                    "enabled": true
                }]
            }),
        )
        .unwrap();

    let disabled = post_json(
        Router::new().merge(routes()).with_state(state.clone()),
        "/settings/skills/toggle",
        serde_json::json!({ "skillId": "toggle-skill", "enabled": false }),
    )
    .await;
    assert_eq!(disabled["enabled"], serde_json::json!(false));
    assert_eq!(
        state.settings_store.get_section("skillsConfig")["instructionSkills"][0]["enabled"],
        serde_json::json!(false)
    );

    let library = get_json(
        Router::new().merge(routes()).with_state(state.clone()),
        "/settings/skills/library",
    )
    .await;
    assert_eq!(library["skills"][0]["enabled"], serde_json::json!(false));

    let enabled = post_json(
        Router::new().merge(routes()).with_state(state.clone()),
        "/settings/skills/toggle",
        serde_json::json!({ "skillId": "toggle-skill", "enabled": true }),
    )
    .await;
    assert_eq!(enabled["enabled"], serde_json::json!(true));
    assert_eq!(
        state.settings_store.get_section("skillsConfig")["instructionSkills"][0]["enabled"],
        serde_json::json!(true)
    );
}

#[tokio::test]
async fn skill_library_reports_unavailable_local_instruction_skills() {
    let state = test_state();
    let valid_dir = tempfile::tempdir().expect("temp skill dir should create");
    std::fs::write(
        valid_dir.path().join("SKILL.md"),
        "# valid-skill\n\n请输出 valid-skill。\n",
    )
    .expect("skill markdown should write");
    let missing_dir = valid_dir.path().join("missing-skill");
    state
        .settings_store
        .set_section(
            "skillsConfig",
            serde_json::json!({
                "instructionSkills": [
                    {
                        "skillId": "valid-skill",
                        "name": "valid-skill",
                        "directoryPath": valid_dir.path().to_string_lossy().to_string()
                    },
                    {
                        "skillId": "missing-skill",
                        "name": "missing-skill",
                        "directoryPath": missing_dir.to_string_lossy().to_string()
                    }
                ]
            }),
        )
        .unwrap();
    let app = Router::new().merge(routes()).with_state(state);

    let body = get_json(app, "/settings/skills/library").await;
    let skills = body["skills"]
        .as_array()
        .expect("skills should be an array");

    assert_eq!(skills.len(), 2);
    let valid = skills
        .iter()
        .find(|skill| skill["skillId"] == serde_json::json!("valid-skill"))
        .expect("valid skill should remain visible");
    assert_eq!(valid["installed"], serde_json::json!(true));
    assert!(valid.get("directoryPath").is_none());
    let missing = skills
        .iter()
        .find(|skill| skill["skillId"] == serde_json::json!("missing-skill"))
        .expect("missing skill should remain manageable");
    assert_eq!(missing["sourceStatus"], serde_json::json!("missing"));
    assert_eq!(missing["updateStatus"], serde_json::json!("source_missing"));
}

#[tokio::test]
async fn instruction_skill_detail_rejects_unavailable_source() {
    let state = test_state();
    let missing_dir = std::env::temp_dir().join(format!("magi-missing-skill-{}", epoch_ms_now()));
    state
        .settings_store
        .set_section(
            "skillsConfig",
            serde_json::json!({
                "instructionSkills": [
                    {
                        "skillId": "missing-skill",
                        "name": "missing-skill",
                        "directoryPath": missing_dir.to_string_lossy().to_string()
                    }
                ]
            }),
        )
        .unwrap();

    let error = get_instruction_skill_detail(
        State(state),
        Query(HashMap::from([(
            "skillId".to_string(),
            "missing-skill".to_string(),
        )])),
    )
    .await
    .expect_err("missing skill source should be rejected");

    assert_eq!(error.message(), "技能源不可用，请重新导入该 Skill");
    assert!(!error.message().contains("magi-missing-skill"));
}

#[tokio::test]
async fn instruction_skill_detail_returns_full_instruction_and_relative_file_list() {
    let state = test_state();
    let dir = std::env::temp_dir().join(format!("magi-skill-detail-{}", epoch_ms_now()));
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!(
            "---\nname: demo\ndescription: 演示\n---\n# 演示\n{}",
            "正文。".repeat(200)
        ),
    )
    .unwrap();
    std::fs::write(dir.join("scripts/run.sh"), "echo ok\n").unwrap();
    std::fs::write(dir.join(".hidden"), "x").unwrap();
    state
        .settings_store
        .set_section(
            "skillsConfig",
            serde_json::json!({
                "instructionSkills": [{
                    "skillId": "demo-skill",
                    "name": "demo",
                    "directoryPath": dir.to_string_lossy().to_string()
                }]
            }),
        )
        .unwrap();

    let Json(detail) = get_instruction_skill_detail(
        State(state),
        Query(HashMap::from([(
            "skillId".to_string(),
            "demo-skill".to_string(),
        )])),
    )
    .await
    .expect("detail should load");

    assert_eq!(detail["instructionFile"], "SKILL.md");
    assert_eq!(detail["truncated"], false);
    assert!(detail["instruction"].as_str().unwrap().contains("# 演示"));
    let files: Vec<&str> = detail["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        files,
        ["SKILL.md", "scripts/run.sh"],
        "只列相对路径，隐藏文件不列"
    );
    assert!(
        !detail.to_string().contains("magi-skill-detail"),
        "不得暴露本机绝对路径"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn instruction_skill_operations_reject_legacy_name_fields() {
    let app = Router::new().merge(routes()).with_state(test_state());

    let (update_status, update_body) = post_json_with_status(
        app,
        "/settings/skills/update",
        serde_json::json!({ "skillName": "legacy-skill" }),
    )
    .await;
    assert_eq!(
        update_status,
        StatusCode::BAD_REQUEST,
        "unexpected body: {update_body}"
    );
    assert!(
        update_body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("skillId 不能为空"),
        "unexpected body: {update_body}"
    );

    let preview_error = get_instruction_skill_detail(
        State(test_state()),
        Query(HashMap::from([(
            "skillName".to_string(),
            "legacy-skill".to_string(),
        )])),
    )
    .await
    .expect_err("legacy detail key should be rejected");
    assert_eq!(preview_error.message(), "skillId 不能为空");
}

#[tokio::test]
async fn custom_tool_add_strips_workspace_session_scope_fields() {
    let state = test_state();
    let app = Router::new().merge(routes()).with_state(state.clone());

    let body = post_json(
        app,
        "/settings/skills/custom-tool/add",
        serde_json::json!({
            "name": "example-tool",
            "workspaceId": "workspace-old",
            "workspacePath": "/tmp/old",
            "sessionId": "session-old"
        }),
    )
    .await;

    assert_eq!(body["added"], true);
    let stored = state.settings_store.get_section("skillsConfig");
    assert_scope_fields_absent(&stored["customTools"][0]);
    assert_eq!(
        stored["customTools"][0]["name"],
        serde_json::json!("example-tool")
    );
    assert!(stored["customTools"][0].get("toolName").is_none());
}

#[tokio::test]
async fn custom_tool_add_rejects_legacy_tool_name_field() {
    let app = Router::new().merge(routes()).with_state(test_state());

    let (status, body) = post_json_with_status(
        app,
        "/settings/skills/custom-tool/add",
        serde_json::json!({
            "toolName": "legacy-tool-name"
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("name 不能为空"),
        "unexpected body: {body}"
    );
}

#[tokio::test]
async fn installed_skill_remove_requires_skill_id_contract() {
    for payload in [
        serde_json::json!({
            "source": "custom",
            "toolName": "legacy-tool"
        }),
        serde_json::json!({
            "source": "custom",
            "name": "legacy-tool"
        }),
        serde_json::json!({
            "source": "instruction",
            "skillName": "legacy-skill"
        }),
        serde_json::json!({
            "source": "instruction",
            "name": "legacy-skill"
        }),
    ] {
        let app = Router::new().merge(routes()).with_state(test_state());

        let (status, body) = post_json_with_status(app, "/settings/skills/remove", payload).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("skillId 不能为空"),
            "unexpected body: {body}"
        );
    }
}

#[tokio::test]
async fn mcp_tools_routes_return_recoverable_disconnected_marker() {
    for path in ["/settings/mcp/tools", "/settings/mcp/tools/refresh"] {
        let app = Router::new().merge(routes()).with_state(test_state());
        let body = post_json(app, path, serde_json::json!({ "serverId": "missing-mcp" })).await;

        assert_eq!(body["serverId"], "missing-mcp");
        assert_eq!(body["connected"], false);
        assert_eq!(body["health"], "disconnected");
        assert_eq!(body["error"], MCP_CONNECTION_FAILED_MARKER);
        assert!(
            body["errorDetail"]
                .as_str()
                .is_some_and(|detail| detail.contains("配置不存在")),
            "unexpected body: {body}"
        );
        assert_eq!(body["toolCount"], 0);
        assert!(body["lastCheckedAt"].as_u64().is_some());
        assert_eq!(body["tools"].as_array().map(Vec::len), Some(0));
    }
}

#[test]
fn mcp_error_detail_keeps_cause_and_redacts_credentials() {
    let detail = public_mcp_error_detail(
        "HTTP MCP request failed: connection refused; Authorization: Bearer private-token; sk-secret-token",
    );
    assert!(
        detail.contains("connection refused"),
        "detail was: {detail}"
    );
    assert!(detail.contains("Bearer [redacted]"), "detail was: {detail}");
    assert!(detail.contains("sk-[redacted]"), "detail was: {detail}");
    assert!(!detail.contains("private-token"), "detail was: {detail}");
    assert!(!detail.contains("secret-token"), "detail was: {detail}");
}

#[tokio::test]
async fn disabled_mcp_server_routes_do_not_report_connection_error() {
    let state = test_state();
    state
        .settings_store
        .upsert_array_entry(
            "mcpServers",
            "id",
            &serde_json::json!({
                "id": "disabled-mcp",
                "name": "disabled-mcp",
                "command": "node",
                "enabled": false,
                "error": "/private/path/raw transport error"
            }),
        )
        .unwrap();

    let app = Router::new().merge(routes()).with_state(state.clone());
    let list = get_json(app, "/settings/mcp").await;
    assert_eq!(list["servers"][0]["connected"], false);
    assert_eq!(list["servers"][0]["health"], MCP_DISABLED_HEALTH);
    assert!(list["servers"][0].get("error").is_none());

    for path in ["/settings/mcp/tools", "/settings/mcp/tools/refresh"] {
        let app = Router::new().merge(routes()).with_state(state.clone());
        let body = post_json(app, path, serde_json::json!({ "serverId": "disabled-mcp" })).await;

        assert_eq!(body["serverId"], "disabled-mcp");
        assert_eq!(body["connected"], false);
        assert_eq!(body["health"], MCP_DISABLED_HEALTH);
        assert!(body.get("error").is_none());
        assert_eq!(body["toolCount"], 0);
        assert_eq!(body["tools"].as_array().map(Vec::len), Some(0));
    }
}

#[test]
fn skill_runtime_errors_keep_private_details_out_of_response() {
    let repository_error =
        skill_repository_error("https://github.com/private/repo", "读取仓库失败", "raw 404");
    let cache_error = skill_cache_error(
        "缓存失败",
        std::path::Path::new("/private/cache/path"),
        "permission denied",
    );

    assert_eq!(repository_error.message(), SKILL_REPOSITORY_PUBLIC_ERROR);
    assert_eq!(cache_error.message(), SKILL_CACHE_PUBLIC_ERROR);
    assert!(!repository_error.message().contains("github.com"));
    assert!(!cache_error.message().contains("/private/cache/path"));
}
