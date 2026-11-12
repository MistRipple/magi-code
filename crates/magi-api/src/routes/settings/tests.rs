use super::*;
use crate::model_config::require_orchestrator_session_model;
use axum::http::StatusCode;
use magi_core::{AbsolutePath, EventId, SessionId, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::GovernanceService;
use magi_session_store::{SessionRecord, SessionStore};
use magi_snapshot::SnapshotManager;
use magi_tool_runtime::{
    BuiltinToolName, ExternalMcpServerCatalogEntry, ExternalToolCatalogSnapshot, ToolRegistry,
};
use magi_workspace::WorkspaceStore;
use std::sync::Arc;

fn test_state() -> ApiState {
    test_state_with_external_catalog_snapshot(None)
}

fn main_source(id: &str, models: &[&str]) -> Value {
    json!({
        "id": id,
        "name": id,
        "baseUrl": format!("https://{id}.example.test/v1"),
        "apiKey": format!("sk-{id}"),
        "apiProtocol": "openai_chat",
        "models": models.iter().map(|model| json!({"id": model})).collect::<Vec<_>>(),
    })
}

fn seed_main_sources(state: &ApiState, sources: Vec<Value>) {
    state
        .settings_store
        .set_section("orchestrator", json!({"revision": 1, "sources": sources}))
        .unwrap();
}

#[tokio::test]
async fn reasoning_metadata_is_connection_scoped_and_ignores_obsolete_overrides() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/v1/models", get(|| async {
                Json(json!({"data":[
                    {"id":"glm-alias","supportedReasoningEfforts":["low","high","max"],"defaultReasoningEffort":"high"},
                    {"id":"gpt-alias","supportedReasoningEfforts":["none","low","medium","high","xhigh","max"],"defaultReasoningEffort":"medium"},
                    {"id":"unknown-alias"}
                ]}))
            }))).await.unwrap();
    });
    let state = test_state();
    let config = json!({"baseUrl":address,"apiKey":"test-key","apiProtocol":"openai_chat"});
    let response = fetch_models(
        State(state.clone()),
        Json(FetchModelsRequest {
            config: config.clone(),
            target: "orch".into(),
            workspace_id: None,
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(
        response["modelCapabilities"]["glm-alias"]["supportedEfforts"],
        json!(["low", "high", "max"])
    );
    assert_eq!(
        response["modelCapabilities"]["gpt-alias"]["supportedEfforts"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    assert_eq!(
        response["modelCapabilities"]["unknown-alias"]["source"],
        "unknown"
    );
    let mut execution = config.clone();
    execution["model"] = json!("glm-alias");
    execution["reasoningEffort"] = json!("xhigh");
    let resolved =
        NormalizedModelConfig::from_settings_with_catalog(&state.settings_store, &execution)
            .unwrap();
    assert_eq!(resolved.reasoning_effort_label(), Some("high"));
    execution["apiKey"] = json!("another-account");
    assert_eq!(
        NormalizedModelConfig::from_settings_with_catalog(&state.settings_store, &execution)
            .unwrap()
            .reasoning_effort_label(),
        None
    );
    execution["apiKey"] = json!("test-key");
    // 能力仅来自服务商目录和受维护的内置目录，旧手工规则不再参与解析。
    execution["modelReasoningOverrides"] = json!({"glm-alias":{"supportedEfforts":["ultra"],"defaultEffort":"ultra","transport":"openai","source":"override"}});
    assert_eq!(
        NormalizedModelConfig::from_settings_with_catalog(&state.settings_store, &execution)
            .unwrap()
            .reasoning_effort_label(),
        Some("high")
    );
    server.abort();
}

#[test]
fn model_catalog_maps_explicit_provider_display_names_without_decoding_aliases() {
    let standard = json!({"data":[{"id":"opaque-gpt","display_name":"GPT Sol"},{"id":"opaque-ambiguous","display_name":"Duplicate"},{"id":"glm-5.3"}]});
    let metadata = json!({"models":[
        {"slug":"gpt-sol","display_name":"GPT Sol","supported_reasoning_levels":[{"effort":"low"},{"effort":"ultra"}],"default_reasoning_level":"low"},
        {"slug":"one","display_name":"Duplicate","supported_reasoning_levels":[{"effort":"low"}]},
        {"slug":"two","display_name":"Duplicate","supported_reasoning_levels":[{"effort":"high"}]},
        {"slug":"glm-5.3","supported_reasoning_levels":[{"effort":"low"},{"effort":"high"},{"effort":"max"}],"default_reasoning_level":"max"}
    ]});
    let mut catalog = ModelCatalog::from_payload(&standard, true);
    catalog.supplement(&standard, &metadata, true);
    assert_eq!(
        catalog.models,
        vec!["glm-5.3", "opaque-ambiguous", "opaque-gpt"]
    );
    assert_eq!(
        catalog.capabilities["opaque-gpt"]["supportedEfforts"],
        json!(["low", "ultra"])
    );
    assert_eq!(catalog.capabilities["glm-5.3"]["defaultEffort"], "max");
    assert!(!catalog.capabilities.contains_key("opaque-ambiguous"));
    assert_eq!(catalog.display_names["opaque-gpt"], "GPT Sol");
    assert_eq!(catalog.display_names["gpt-sol"], "GPT Sol");
    assert_eq!(
        parse_model_ids(&metadata),
        vec!["glm-5.3", "gpt-sol", "one", "two"]
    );
}

#[tokio::test]
async fn fetch_models_preserves_ids_and_refreshes_provider_display_names() {
    let payload = std::sync::Arc::new(std::sync::RwLock::new(json!({"data":[
        {"id":"opaque-id","display_name":" GPT Sol ","supportedEfforts":[]},
        {"id":"camel-id","displayName":"Gemini Flash","supportedEfforts":[]},
        {"id":"plain-id","display_name":"  ","supportedEfforts":[]}
    ]})));
    let served = payload.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/models",
                get(move || {
                    let snapshot = served.read().unwrap().clone();
                    async move { Json(snapshot) }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let state = test_state();
    let config = json!({"baseUrl":address,"apiKey":"test-key","apiProtocol":"anthropic_messages"});
    let fetch = || {
        fetch_models(
            State(state.clone()),
            Json(FetchModelsRequest {
                config: config.clone(),
                target: "orch".into(),
                workspace_id: None,
            }),
        )
    };
    let first = fetch().await.unwrap().0;
    assert_eq!(
        first["models"],
        json!(["camel-id", "opaque-id", "plain-id"])
    );
    assert_eq!(
        first["modelDisplayNames"],
        json!({"opaque-id":"GPT Sol","camel-id":"Gemini Flash"})
    );
    assert!(first.get("orchestratorSessionDefaults").is_none());
    // A refresh replaces the whole projection, including renamed/removed display names.
    *payload.write().unwrap() = json!({"data":[
        {"id":"opaque-id","display_name":"GPT Sol Updated","supportedEfforts":[]},
        {"id":"camel-id","supportedEfforts":[]},
        {"id":"new-id","display_name":"New Model","supportedEfforts":[]}
    ]});
    let refreshed = fetch().await.unwrap().0;
    assert_eq!(
        refreshed["models"],
        json!(["camel-id", "new-id", "opaque-id"])
    );
    assert_eq!(
        refreshed["modelDisplayNames"],
        json!({"opaque-id":"GPT Sol Updated","new-id":"New Model"})
    );
    assert!(
        state
            .settings_store
            .get_section("orchestratorSessionDefaults")
            .is_null(),
        "模型目录探测不得写会话默认值"
    );
    server.abort();
}

#[tokio::test]
async fn reasoning_catalog_negotiates_codex_metadata_for_both_http_protocols() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/v1/models", get(|Query(query): Query<HashMap<String,String>>| async move {
                if query.contains_key("client_version") {
                    Json(json!({"models":[{"slug":"glm-5.3","supported_reasoning_levels":[{"effort":"low"},{"effort":"high"},{"effort":"max"}],"default_reasoning_level":"max"}]}))
                } else { Json(json!({"data":[{"id":"glm-5.3"}]})) }
            }))).await.unwrap();
    });
    for protocol in ["openai_responses", "anthropic_messages"] {
        let state = test_state();
        let config = json!({"baseUrl":address,"apiKey":"test-key","apiProtocol":protocol});
        let response = model_reasoning_capability(
            State(state.clone()),
            Json(json!({"config":config,"model":"glm-5.3"})),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(response["supportedEfforts"], json!(["low", "high", "max"]));
        assert_eq!(response["defaultEffort"], "max");
        assert_eq!(response["source"], "provider");
        assert_eq!(
            response["transport"],
            if protocol == "anthropic_messages" {
                "anthropic_effort"
            } else {
                "openai"
            }
        );
        let mut execution = config.clone();
        execution["model"] = json!("glm-5.3");
        execution["reasoningEffort"] = json!("high");
        let runtime =
            NormalizedModelConfig::from_settings_with_catalog(&state.settings_store, &execution)
                .unwrap();
        assert_eq!(runtime.reasoning_effort_label(), Some("high"));
    }
    server.abort();
}

#[tokio::test]
async fn optional_model_metadata_failure_keeps_standard_catalog_usable() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/models",
                get(|Query(query): Query<HashMap<String, String>>| async move {
                    if query.contains_key("client_version") {
                        (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error":"unsupported parameter"})),
                        )
                    } else {
                        (StatusCode::OK, Json(json!({"data":[{"id":"plain-model"}]})))
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let config = NormalizedModelConfig::from_settings_value(
        &json!({"baseUrl":address,"apiKey":"test-key","apiProtocol":"openai_chat"}),
    )
    .unwrap();
    let catalog = fetch_model_catalog_for_config(&config).await.unwrap();
    assert_eq!(catalog.models, vec!["plain-model"]);
    assert!(catalog.capabilities.is_empty());
    server.abort();
}

fn role_test_state(role_dir: &std::path::Path) -> ApiState {
    test_state().with_agent_role_registry(Arc::new(
        magi_agent_role::AgentRoleRegistry::builtin().with_user_role_dir(role_dir),
    ))
}

fn role_payload(id: &str) -> Value {
    json!({
        "role": {
            "id": id,
            "displayName": "数据分析师",
            "description": "负责清洗、分析和验证数据",
            "systemPrompt": "你是数据分析师，先核对数据契约，再输出可复现结论。",
            "supportedKinds": ["local_agent"],
            "parallelismLimit": 2,
            "coordinatorMode": false,
            "version": 1,
            "profile": {
                "role": "数据分析与验证",
                "focus": ["读取,清洗", "指标核对"],
                "constraints": ["保留证据"],
                "outputPreferences": ["数据口径", "验证结果"]
            },
            "ownerships": ["分析结论"],
            "insightPreferences": ["decision", "risk"],
            "capabilities": ["general_engineering", "data_engineering"],
            "defaultUI": {"colorToken": "agent-data-analyst", "icon": "bar-chart"}
        }
    })
}

fn role_export_content(state: &ApiState, role_id: &str) -> String {
    let role = state
        .agent_role_registry
        .get(role_id)
        .expect("测试角色应存在");
    magi_agent_role::serialize_role_markdown(&role)
}

#[test]
fn role_templates_publish_non_empty_professional_capabilities() {
    let registry = magi_agent_role::AgentRoleRegistry::load_default();
    let templates = role_templates_for_registry(&registry);
    let builtin_templates =
        role_templates_for_registry(&magi_agent_role::AgentRoleRegistry::builtin());

    // 默认注册表会加载用户自定义角色，数量可以多于内置角色；这里只要求
    // 所有内置模板仍然发布，避免测试依赖本机 ~/.magi 中是否存在用户角色。
    let template_ids = templates
        .iter()
        .filter_map(|template| template["templateId"].as_str())
        .collect::<std::collections::HashSet<_>>();
    for builtin_template in &builtin_templates {
        assert!(
            template_ids.contains(builtin_template["templateId"].as_str().unwrap()),
            "内置角色模板必须保留"
        );
    }
    for template in templates {
        let template_id = template["templateId"]
            .as_str()
            .expect("角色模板必须包含 templateId");
        let capabilities = template["capabilities"]
            .as_array()
            .expect("角色模板必须包含能力数组");
        assert!(!capabilities.is_empty(), "{template_id} 缺少专业能力");
        assert!(
            capabilities
                .iter()
                .all(|capability| capability["version"].as_u64().is_some()),
            "{template_id} 的能力必须带版本"
        );
    }
}

fn test_state_with_external_catalog_snapshot(
    external_catalog_snapshot: Option<ExternalToolCatalogSnapshot>,
) -> ApiState {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let governance = Arc::new(GovernanceService::default());
    let workspace_store = Arc::new(WorkspaceStore::default());
    let session_store = Arc::new(SessionStore::default());
    let snapshot_manager = Arc::new(SnapshotManager::new());
    let runtime_capability_dependency_provider =
        crate::state::build_runtime_capability_dependency_provider(true);
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus))
        .with_runtime_capability_dependency_provider(runtime_capability_dependency_provider);
    if let Some(snapshot) = external_catalog_snapshot {
        tool_registry =
            tool_registry.with_external_tool_catalog_provider(Arc::new(move || snapshot.clone()));
    }
    tool_registry.register_default_builtins();
    ApiState::new(
        "magi-test",
        event_bus,
        session_store,
        workspace_store,
        governance,
    )
    .with_snapshot_manager(snapshot_manager)
    .with_tool_registry(tool_registry)
}

fn register_test_workspace(state: &ApiState, workspace_id: &str) -> tempfile::TempDir {
    let workspace_root = tempfile::Builder::new()
        .prefix(&format!("magi-settings-{workspace_id}-"))
        .tempdir()
        .expect("workspace root should create");
    let workspace_path = workspace_root.path().to_string_lossy().to_string();
    state
        .workspace_registry
        .register(
            WorkspaceId::new(workspace_id),
            AbsolutePath::new(&workspace_path),
        )
        .expect("workspace should register");
    workspace_root
}

fn test_session_record(session_id: &SessionId, workspace_id: &str, title: &str) -> SessionRecord {
    let now = UtcMillis::now();
    SessionRecord {
        session_id: session_id.clone(),
        title: title.to_string(),
        status: magi_core::SessionLifecycleStatus::Active,
        created_at: now,
        updated_at: now,
        message_count: None,
        workspace_id: Some(workspace_id.to_string()),
        last_completed_at: None,
        last_viewed_at: None,
        kind: Default::default(),
    }
}

fn seed_session(state: &ApiState, session: SessionRecord) {
    state
        .session_store
        .create_session_for_workspace(
            session.session_id.clone(),
            session.title,
            session.workspace_id,
        )
        .expect("test session should create");
}

fn model_usage_payload(
    event_id: &str,
    workspace_id: &str,
    session_id: &str,
    call_id: &str,
    timestamp: u64,
    input_tokens: u64,
    output_tokens: u64,
) -> Value {
    serde_json::json!({
        "workspaceId": workspace_id,
        "sessionId": session_id,
        "turnId": "turn-1",
        "eventId": event_id,
        "timestamp": timestamp,
        "executionBinding": {
            "templateId": "orchestrator",
            "engineId": "orchestrator",
            "bindingRevision": 0,
            "role": "orchestrator"
        },
        "modelConfig": {
            "provider": "openai",
            "model": "gpt-4.1",
            "baseUrl": "https://api.openai.com/v1",
        },
        "callIdentity": {
            "callId": call_id,
            "source": "orchestrator",
            "phase": "planning"
        },
        "usage": {
            "inputTokens": input_tokens,
            "outputTokens": output_tokens,
            "cacheReadTokens": 4,
            "cacheWriteTokens": 3
        },
        "status": "success"
    })
}

async fn anthropic_probe_stub(
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> impl axum::response::IntoResponse {
    assert_eq!(
        headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("test-key")
    );
    assert_eq!(
        headers
            .get("anthropic-version")
            .and_then(|value| value.to_str().ok()),
        Some("2023-06-01")
    );
    assert_eq!(payload["model"], json!("claude-sonnet-test"));
    // 末条 user 消息带滚动缓存断点，内容以等价 text block 发送。
    assert_eq!(payload["messages"][0]["content"][0]["text"], json!("ping"));
    (
        StatusCode::OK,
        [(CONTENT_TYPE, "text/event-stream")],
        concat!(
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"pong\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        ),
    )
}

async fn openai_responses_probe_stub(
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> impl axum::response::IntoResponse {
    assert_eq!(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer test-key")
    );
    assert_eq!(payload["model"], json!("gpt-5-test"));
    assert_eq!(payload["input"][0]["type"], json!("message"));
    assert_eq!(
        payload["input"][0]["content"][0]["type"],
        json!("input_text")
    );
    (
        StatusCode::OK,
        [(CONTENT_TYPE, "text/event-stream")],
        concat!(
            "event: response.output_text.delta\n",
            "data: {\"delta\":\"pong\"}\n\n",
            "event: response.completed\n",
            "data: {\"response\":{\"status\":\"completed\"}}\n\n",
        ),
    )
}

async fn rejected_probe_stub() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": {
                "message": "provider rejected: /Users/xie/.magi/token"
            }
        })),
    )
}

async fn rejected_models_stub() -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": {
                "message": "upstream crashed at /var/tmp/provider.log"
            }
        })),
    )
}

async fn successful_models_stub() -> Json<Value> {
    Json(json!({
        "data": [
            { "id": "gpt-test" }
        ]
    }))
}

async fn image_generation_probe_stub(
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Json<Value> {
    assert_eq!(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer test-image-key")
    );
    assert_eq!(payload["model"], json!("gpt-image-test"));
    assert_eq!(
        payload["prompt"],
        json!("A simple blue square on a white background")
    );
    assert_eq!(payload["response_format"], json!("b64_json"));
    Json(json!({
        "data": [{
            "b64_json": "iVBORw0KGgo=",
            "revised_prompt": "a blue square"
        }]
    }))
}

#[tokio::test]
async fn runtime_setting_update_accepts_only_typed_runtime_preferences() {
    let state = test_state();
    let saved = update_setting(
        State(state.clone()),
        Json(json!({ "key": "conversationDisplayMode", "value": "summary" })),
    )
    .await
    .expect("display mode should be saved")
    .0;
    assert_eq!(saved["conversationDisplayMode"], json!("summary"));
    assert_eq!(
        state.settings_runtime_json()["conversationDisplayMode"],
        json!("summary")
    );

    for rejected in [
        json!({ "key": "conversationDisplayMode", "value": "compact" }),
        json!({ "key": "locale", "value": "fr-FR" }),
        json!({ "key": "browser", "value": {} }),
        json!({ "key": "runtimeSettings", "value": { "conversationDisplayMode": "original" } }),
    ] {
        assert!(
            update_setting(State(state.clone()), Json(rejected.clone()))
                .await
                .is_err(),
            "{rejected} must be rejected"
        );
    }
    assert_eq!(
        state.settings_runtime_json()["conversationDisplayMode"],
        json!("summary"),
        "rejected updates must not change the stored preference"
    );
}

#[tokio::test]
async fn connection_probe_supports_anthropic_messages_api() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub listener should bind");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("stub addr should exist")
    );
    let app = Router::new().route("/v1/messages", post(anthropic_probe_stub));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("stub server should run");
    });

    let config = NormalizedModelConfig::from_settings_value(&json!({
        "baseUrl": base_url,
        "apiKey": "test-key",
        "model": "claude-sonnet-test",
        "apiProtocol": "anthropic_messages"
    }))
    .expect("模型配置应符合当前协议");
    execute_connection_probe(&config)
        .await
        .expect("anthropic probe should succeed");
    server.abort();
}

#[tokio::test]
async fn connection_probe_supports_openai_responses_api() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub listener should bind");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("stub addr should exist")
    );
    let app = Router::new().route("/v1/responses", post(openai_responses_probe_stub));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("stub server should run");
    });

    let config = NormalizedModelConfig::from_settings_value(&json!({
        "baseUrl": base_url,
        "apiKey": "test-key",
        "model": "gpt-5-test",
        "apiProtocol": "openai_responses"
    }))
    .expect("模型配置应符合 Responses 协议");
    execute_connection_probe(&config)
        .await
        .expect("Responses probe should succeed");
    server.abort();
}

#[tokio::test]
async fn connection_probe_redacts_remote_error_message() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub listener should bind");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("stub addr should exist")
    );
    let app = Router::new().route("/v1/chat/completions", post(rejected_probe_stub));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("stub server should run");
    });

    let result = probe_connection_response(
        &test_state(),
        json!({
            "baseUrl": base_url,
            "apiKey": "test-key",
            "model": "gpt-test",
            "apiProtocol": "openai_chat"
        }),
    )
    .await;

    match result {
        Err(ApiError::InvalidInput(message)) => {
            assert_eq!(message, "模型鉴权失败");
            assert!(!message.contains("provider rejected"));
            assert!(!message.contains("/Users/xie"));
        }
        other => panic!("unexpected probe result: {:?}", other),
    }
    server.abort();
}

#[tokio::test]
async fn orchestrator_test_without_model_uses_catalog_probe() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub listener should bind");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("stub addr should exist")
    );
    let app = Router::new().route("/v1/models", get(successful_models_stub));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("stub server should run");
    });

    let result = test_orchestrator_connection(
        State(test_state()),
        Json(json!({
            "baseUrl": base_url,
            "apiKey": "test-key",
            "apiProtocol": "openai_chat"
        })),
    )
    .await;

    let _ = result.expect("orchestrator connection-only probe should pass through model catalog");
    server.abort();
}

#[tokio::test]
async fn session_defaults_project_first_catalog_model_until_user_selects_one() {
    let state = test_state();
    state
        .settings_store
        .set_section(
            "orchestrator",
            json!({"revision": 1, "sources": [
                main_source("one", &["z-test", "gpt-test"]),
                main_source("two", &["other"])
            ]}),
        )
        .unwrap();
    let defaults = crate::model_config::orchestrator_session_defaults(&state);
    assert_eq!(defaults["sourceId"], "custom/one");
    assert_eq!(defaults["model"], "z-test");
    assert!(defaults["reasoningEffort"].is_null());
    assert!(
        state
            .settings_store
            .get_section(ORCHESTRATOR_SESSION_DEFAULTS_SECTION)
            .is_null(),
        "投影第一项不得写入持久化默认值"
    );
    assert_eq!(
        crate::model_config::initial_session_orchestrator_config(&state, true, None).unwrap(),
        Some(defaults),
        "首条请求继承目录第一项"
    );

    state
        .settings_store
        .set_section(
            ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
            json!({"sourceId": "custom/two", "model": "other", "reasoningEffort": "high"}),
        )
        .unwrap();
    let selected = crate::model_config::orchestrator_session_defaults(&state);
    assert_eq!(selected["sourceId"], "custom/two");
    assert_eq!(selected["model"], "other");
}

#[tokio::test]
async fn orchestrator_test_with_model_uses_real_invocation_probe() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub listener should bind");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("stub addr should exist")
    );
    let app = Router::new().route("/v1/chat/completions", post(rejected_probe_stub));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("stub server should run");
    });

    let result = test_orchestrator_connection(
        State(test_state()),
        Json(json!({
            "baseUrl": base_url,
            "apiKey": "test-key",
            "model": "gpt-test",
            "apiProtocol": "openai_chat"
        })),
    )
    .await;

    match result {
        Err(ApiError::InvalidInput(message)) => assert_eq!(message, "模型鉴权失败"),
        other => panic!("unexpected orchestrator probe result: {:?}", other),
    }
    server.abort();
}

#[tokio::test]
async fn image_generation_test_uses_real_standard_endpoint() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub listener should bind");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("stub addr should exist")
    );
    let app = Router::new().route("/v1/images/generations", post(image_generation_probe_stub));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("stub server should run");
    });

    let result = test_image_generation_connection(
        State(test_state()),
        Json(json!({
            "baseUrl": base_url,
            "apiKey": "test-image-key",
            "model": "gpt-image-test",
            "apiProtocol": "openai_chat"
        })),
    )
    .await
    .expect("image generation probe should succeed");

    assert_eq!(result.0["success"], json!(true));
    assert_eq!(result.0["mediaType"], json!("image/png"));
    assert_eq!(result.0["bytes"], json!(8));
    server.abort();
}

#[tokio::test]
async fn fetch_models_redacts_remote_error_message() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub listener should bind");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("stub addr should exist")
    );
    let app = Router::new().route("/v1/models", get(rejected_models_stub));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("stub server should run");
    });

    let result = fetch_models(
        State(test_state()),
        Json(FetchModelsRequest {
            config: json!({
                "baseUrl": base_url,
                "apiKey": "test-key",
                "model": "gpt-test",
                "apiProtocol": "openai_chat"
            }),
            target: "orchestrator".to_string(),
            workspace_id: None,
        }),
    )
    .await;

    match result {
        Err(ApiError::InvalidInput(message)) => {
            assert_eq!(message, "模型服务暂不可用");
            assert!(!message.contains("upstream crashed"));
            assert!(!message.contains("/var/tmp"));
        }
        other => panic!("unexpected model fetch result: {:?}", other),
    }
    server.abort();
}

#[tokio::test]
async fn settings_bootstrap_returns_frontend_contract_sections() {
    let state = test_state();
    let workspace_id = WorkspaceId::new("workspace-contract");
    let workspace_root =
        std::env::temp_dir().join(format!("magi-settings-contract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workspace_root);
    std::fs::create_dir_all(&workspace_root).expect("workspace root should create");
    let workspace_path = workspace_root.to_string_lossy().to_string();
    state
        .workspace_registry
        .register(workspace_id.clone(), AbsolutePath::new(&workspace_path))
        .expect("workspace should register");
    let session_id = SessionId::new("session-empty-contract");
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "设置契约会话",
            Some(workspace_id.to_string()),
        )
        .expect("session should create");
    state.event_bus.publish(EventEnvelope::audit(
        EventId::new("event-settings-safety-audit"),
        "security.safety.evaluated",
        json!({"decision": "audit_only"}),
    ));
    state.event_bus.publish(EventEnvelope::audit(
        EventId::new("event-settings-unrelated-audit"),
        "context.reference.read",
        json!({"path": "README.md"}),
    ));

    let bootstrap = settings_bootstrap(
        State(state),
        Query(HashMap::from([
            ("scope".to_string(), "workspace".to_string()),
            ("sessionId".to_string(), session_id.as_str().to_string()),
            ("workspaceId".to_string(), workspace_id.to_string()),
            ("workspacePath".to_string(), workspace_path.clone()),
            ("accessProfile".to_string(), "full_access".to_string()),
        ])),
    )
    .await
    .expect("settings bootstrap should build")
    .0;
    let object = bootstrap
        .as_object()
        .expect("settings bootstrap should be an object");

    for key in [
        "workerConfigs",
        "orchestratorConfig",
        "auxiliaryConfig",
        "imageGenerationConfig",
        "userRulesConfig",
        "skillsConfig",
        "safeguardConfig",
        "workerStatuses",
        "runtimeSettings",
    ] {
        assert!(bootstrap[key].is_object(), "{key} should be an object");
    }
    assert_eq!(bootstrap["workspaceId"], json!("workspace-contract"));
    assert_eq!(bootstrap["sessionId"], json!("session-empty-contract"));
    assert_eq!(
        bootstrap["workspacePath"],
        json!(
            std::path::Path::new(&workspace_path)
                .canonicalize()
                .unwrap_or_else(|_| std::path::PathBuf::from(workspace_path))
                .to_string_lossy()
        )
    );
    assert_eq!(bootstrap["safeguardAudit"]["auditCount"], json!(1));
    for key in [
        "repositories",
        "mcpServers",
        "builtinTools",
        "capabilityDependencies",
        "roleTemplates",
        "registryEngines",
        "registryAgents",
        "visionBuiltinTextModelRules",
    ] {
        assert!(bootstrap[key].is_array(), "{key} should be an array");
    }
    assert!(
        bootstrap["visionBuiltinTextModelRules"]
            .as_array()
            .expect("vision rules should be an array")
            .iter()
            .any(|rule| rule["displayName"] == json!("GLM 5.2")),
        "vision rules should retain GLM 5.2"
    );
    let builtin_tools = bootstrap["builtinTools"]
        .as_array()
        .expect("builtin tools should be an array");
    let builtin_names: Vec<_> = builtin_tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    let expected_builtin_names = vec![
        "file_read",
        "view_image",
        "file_write",
        "file_patch",
        "apply_patch",
        "file_remove",
        "file_mkdir",
        "file_copy",
        "file_move",
        "search_text",
        "search_semantic",
        "shell_exec",
        "process_inspect",
        "diff_preview",
        "web_search",
        "web_fetch",
        "browser_navigate",
        "browser_snapshot",
        "browser_click",
        "browser_type",
        "browser_press",
        "browser_scroll",
        "browser_screenshot",
        "browser_tabs",
        "browser_viewport",
        "browser_wait_for",
        "browser_hover",
        "browser_drag",
        "browser_fill_form",
        "browser_dialog",
        "browser_upload_file",
        "browser_click_at",
        "browser_evaluate",
        "browser_console",
        "browser_network",
        "browser_emulate",
        "browser_performance",
        "browser_lighthouse",
        "browser_heap",
        "browser_third_party",
        "browser_webmcp",
        "browser_pwa",
        "browser_read",
        "browser_storage",
        "browser_download",
        "diagram_render",
        "image_generate",
        "knowledge_query",
        "knowledge_graph_query",
        "code_symbols",
        "tool_catalog",
        "git_status",
        "git_branch_list",
        "git_branch_create",
        "git_branch_switch",
        "git_pull",
        "git_push",
        "git_merge_preview",
        "git_merge",
        "git_branch_delete",
        "git_worktree_list",
        "git_worktree_create",
        "git_worktree_remove",
        "get_goal",
        "create_goal",
        "update_goal",
        "agent_spawn",
        "agent_send",
        "agent_cancel",
        "agent_apply",
        "agent_wait",
        "context_search",
        "context_read",
        "context_request",
        "update_plan",
        "update_execution_steps",
        "memory_write",
        "ask_user_question",
        "agent_role_get",
        "agent_role_save",
    ];
    assert_eq!(
        builtin_names, expected_builtin_names,
        "bootstrap must expose one canonical public builtin surface in a stable order"
    );
    let builtin_categories = builtin_tools
        .iter()
        .map(|tool| tool["category"].as_str().expect("tool category"))
        .collect::<std::collections::BTreeSet<_>>();
    let expected_category_count = BuiltinToolName::ALL
        .iter()
        .filter(|tool| tool.is_public_tool_surface())
        .map(BuiltinToolName::category)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    assert_eq!(builtin_categories.len(), expected_category_count);
    assert!(
        builtin_tools
            .iter()
            .filter(|tool| tool["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("git_")))
            .all(|tool| tool["category"] == serde_json::json!("git")),
        "all Git actions must belong to one Git capability category"
    );
    assert!(
        builtin_tools
            .iter()
            .any(|tool| tool["name"] == serde_json::json!("shell_exec")),
        "builtin tools should expose shell_exec"
    );
    let shell_exec = builtin_tools
        .iter()
        .find(|tool| tool["name"] == serde_json::json!("shell_exec"))
        .expect("shell_exec should be exposed");
    assert_eq!(shell_exec["runtimeStatus"], serde_json::json!("ready"));
    assert_eq!(
        shell_exec["policyScope"],
        serde_json::json!("input_sensitive"),
        "settings bootstrap should preserve tool catalog policy scope"
    );
    assert_eq!(
        shell_exec["inputSensitivePolicy"],
        serde_json::json!(true),
        "settings bootstrap should distinguish input-sensitive invocation policy"
    );
    assert_eq!(
        shell_exec["effectiveApprovalPolicy"],
        serde_json::json!("regular_risk_block_skipped"),
        "settings bootstrap should render tool policy under requested access profile"
    );
    assert_eq!(
        shell_exec["accessProfileBehavior"],
        serde_json::json!("full_access_skips_regular_risk_blocks"),
        "settings bootstrap should keep access profile diagnostics aligned with composer mode"
    );
    assert_eq!(
        shell_exec["runtimeInternal"],
        serde_json::json!(false),
        "ordinary public tools should not be marked as task-runtime-only"
    );
    let agent_spawn = builtin_tools
        .iter()
        .find(|tool| tool["name"] == serde_json::json!("agent_spawn"))
        .expect("agent_spawn should be exposed for task runtime");
    assert_eq!(
        agent_spawn["runtimeInternal"],
        serde_json::json!(true),
        "task protocol tools should be distinguishable from ordinary local tools"
    );
    let knowledge_query = builtin_tools
        .iter()
        .find(|tool| tool["name"] == serde_json::json!("knowledge_query"))
        .expect("knowledge_query should be exposed");
    assert_eq!(
        knowledge_query["runtimeStatus"],
        serde_json::json!("unavailable"),
        "settings bootstrap must surface runtime health from tool_catalog"
    );
    assert_eq!(
        knowledge_query["runtimeWarnings"],
        serde_json::json!(["runtime_warning"]),
        "settings bootstrap should expose warning markers instead of raw runtime details"
    );
    assert!(
        !knowledge_query.to_string().contains("知识检索能力暂不可用"),
        "settings bootstrap should not expose raw runtime warning text"
    );
    let capability_dependencies = bootstrap["capabilityDependencies"]
        .as_array()
        .expect("capability dependencies should be an array");
    assert_eq!(capability_dependencies.len(), 8);
    assert_eq!(
        capability_dependencies[0]["name"],
        serde_json::json!("knowledge_store")
    );
    assert_eq!(
        capability_dependencies[0]["status"],
        serde_json::json!("unavailable")
    );
    assert_eq!(
        capability_dependencies[0]["requiredBy"],
        serde_json::json!([
            "knowledge_query",
            "knowledge_graph_query",
            "search_semantic",
            "code_symbols"
        ])
    );
    assert!(
        capability_dependencies[0].get("required_by").is_none(),
        "settings bootstrap should expose dependency fields in frontend camelCase"
    );
    assert_eq!(
        capability_dependencies[1]["name"],
        serde_json::json!("workspace_code_index")
    );
    assert_eq!(
        capability_dependencies[1]["status"],
        serde_json::json!("unavailable")
    );
    assert!(capability_dependencies[1].get("workspaceId").is_none());
    assert_eq!(
        capability_dependencies[2]["name"],
        serde_json::json!("agent_role_registry")
    );
    assert_eq!(
        capability_dependencies[3]["name"],
        serde_json::json!("skill_runtime")
    );
    assert_eq!(
        capability_dependencies[3]["status"],
        serde_json::json!("unavailable"),
        "full settings bootstrap should surface missing external skill runtime diagnostics"
    );
    assert_eq!(
        capability_dependencies[3]["toolCount"],
        serde_json::json!(0)
    );
    assert!(
        capability_dependencies[3].get("tool_count").is_none(),
        "settings bootstrap should expose external dependency counts in frontend camelCase"
    );
    assert_eq!(
        capability_dependencies[4]["name"],
        serde_json::json!("mcp_servers")
    );
    assert_eq!(
        capability_dependencies[4]["status"],
        serde_json::json!("unavailable"),
        "full settings bootstrap should surface missing MCP diagnostics"
    );
    assert_eq!(
        capability_dependencies[4]["readyCount"],
        serde_json::json!(0)
    );
    assert!(
        capability_dependencies[4].get("ready_count").is_none(),
        "settings bootstrap should expose external dependency counts in frontend camelCase"
    );
    assert_eq!(
        capability_dependencies[5]["name"],
        serde_json::json!("image_generation_model")
    );
    assert_eq!(
        capability_dependencies[5]["status"],
        serde_json::json!("unavailable")
    );
    assert_eq!(
        capability_dependencies[5]["requiredBy"],
        serde_json::json!(["image_generate"])
    );
    assert_eq!(
        capability_dependencies[6]["name"],
        serde_json::json!("context_runtime")
    );
    assert_eq!(
        capability_dependencies[6]["status"],
        serde_json::json!("ready")
    );
    assert!(capability_dependencies[6].get("sessionId").is_none());
    assert_eq!(
        capability_dependencies[7]["name"],
        serde_json::json!("file_snapshot")
    );
    assert_eq!(
        capability_dependencies[7]["status"],
        serde_json::json!("ready"),
        "snapshot dependency should report the lazy snapshot capability as ready before the session starts"
    );
    assert!(capability_dependencies[7].get("sessionId").is_none());
    assert!(
        builtin_tools
            .iter()
            .any(|tool| tool["name"] == serde_json::json!("diagram_render")),
        "builtin tools should expose the unified diagram_render surface"
    );
    assert!(
        builtin_tools
            .iter()
            .all(|tool| tool["name"] != serde_json::json!("mermaid_diagram")),
        "mermaid_diagram should not remain a public builtin surface"
    );
    assert!(
        builtin_tools
            .iter()
            .all(|tool| tool["name"] != serde_json::json!("process_launch")),
        "process_launch should remain an internal shell runtime capability"
    );
    for internal_tool in [
        "process_read",
        "process_write",
        "process_kill",
        "process_list",
    ] {
        assert!(
            builtin_tools
                .iter()
                .all(|tool| tool["name"] != serde_json::json!(internal_tool)),
            "{internal_tool} should remain an internal shell runtime capability"
        );
    }
    assert_eq!(bootstrap["userRulesConfig"], serde_json::json!({}));
    assert_eq!(
        bootstrap["runtimeSettings"]["locale"],
        serde_json::json!("zh-CN")
    );
    assert_eq!(bootstrap["bootstrapScope"], serde_json::json!("full"));
    assert_eq!(bootstrap["mcpServersHydrated"], serde_json::json!(true));
    assert!(
        !bootstrap["safeguardConfig"]["rules"]
            .as_array()
            .expect("safeguard rules should be an array")
            .is_empty()
    );
    assert!(!object.contains_key("userRules"));
    assert!(!object.contains_key("engines"));
    assert!(!object.contains_key("agents"));
    let _ = std::fs::remove_dir_all(&workspace_root);
}

#[tokio::test]
async fn settings_bootstrap_reports_product_capabilities_without_tool_context() {
    let state = test_state();
    state
        .settings_store
        .set_section(
            ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
            json!({ "model": "model-last-used", "reasoningEffort": "high" }),
        )
        .unwrap();
    let bootstrap = settings_bootstrap(
        State(state),
        Query(HashMap::from([(
            "scope".to_string(),
            "personal".to_string(),
        )])),
    )
    .await
    .expect("settings bootstrap should build")
    .0;
    assert!(bootstrap["workspaceId"].is_null());
    assert!(bootstrap["workspacePath"].is_null());
    assert!(bootstrap["sessionId"].is_null());
    assert_eq!(
        bootstrap["orchestratorSessionDefaults"],
        json!({ "model": "model-last-used", "reasoningEffort": "high" })
    );
    assert_eq!(
        bootstrap["orchestratorSessionConfig"], bootstrap["orchestratorSessionDefaults"],
        "无会话草稿必须直接读取持久化的新会话默认配置"
    );
    assert_eq!(
        bootstrap["effectiveOrchestratorConfig"]["model"],
        json!("model-last-used")
    );
    let dependencies = bootstrap["capabilityDependencies"]
        .as_array()
        .expect("capability dependencies should be listed");

    for name in ["context_runtime", "file_snapshot"] {
        let dependency = dependencies
            .iter()
            .find(|dependency| dependency["name"] == name)
            .unwrap_or_else(|| panic!("{name} dependency should be listed"));
        assert_eq!(dependency["status"], json!("ready"));
        assert!(dependency.get("workspaceId").is_none());
        assert!(dependency.get("sessionId").is_none());
    }
}

#[tokio::test]
async fn settings_bootstrap_resolves_workspace_from_registered_path() {
    let state = test_state();
    let workspace_id = WorkspaceId::new("workspace-settings-registered");
    let workspace_path = "/tmp/magi-settings-registered";
    state
        .workspace_registry
        .register(workspace_id.clone(), AbsolutePath::new(workspace_path))
        .expect("workspace should register");

    let bootstrap = settings_bootstrap(
        State(state),
        Query(HashMap::from([
            ("scope".to_string(), "workspace".to_string()),
            (
                "workspaceId".to_string(),
                "workspace-stale-query".to_string(),
            ),
            ("workspacePath".to_string(), workspace_path.to_string()),
        ])),
    )
    .await
    .expect("settings bootstrap should build")
    .0;

    assert_eq!(bootstrap["workspaceId"], json!(workspace_id.as_str()));
    assert_eq!(
        bootstrap["workspacePath"],
        json!(
            std::path::Path::new(&workspace_path)
                .canonicalize()
                .unwrap_or_else(|_| std::path::PathBuf::from(workspace_path))
                .to_string_lossy()
        )
    );
}

#[tokio::test]
async fn settings_bootstrap_uses_defaults_for_session_without_override() {
    let state = test_state();
    let session_id = SessionId::new("session-settings-default-model");
    state
        .session_store
        .create_session(session_id.clone(), "继承默认模型")
        .expect("personal session should be creatable");
    state
        .settings_store
        .set_section(
            ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
            json!({ "model": "model-last-used", "reasoningEffort": "xhigh" }),
        )
        .unwrap();

    let bootstrap = settings_bootstrap(
        State(state),
        Query(HashMap::from([
            ("scope".to_string(), "personal".to_string()),
            ("sessionId".to_string(), session_id.to_string()),
        ])),
    )
    .await
    .expect("personal settings bootstrap should build")
    .0;

    assert_eq!(
        bootstrap["orchestratorSessionConfig"],
        json!({ "model": "model-last-used", "reasoningEffort": "xhigh" })
    );
    assert_eq!(
        bootstrap["effectiveOrchestratorConfig"]["model"],
        json!("model-last-used")
    );
}

#[tokio::test]
async fn settings_bootstrap_returns_null_workspace_for_personal_session() {
    let state = test_state();
    let session_id = SessionId::new("session-settings-personal");
    state
        .session_store
        .create_session(session_id.clone(), "个人设置会话")
        .expect("personal session should be creatable");

    let bootstrap = settings_bootstrap(
        State(state),
        Query(HashMap::from([
            ("scope".to_string(), "personal".to_string()),
            ("sessionId".to_string(), session_id.to_string()),
        ])),
    )
    .await
    .expect("personal settings bootstrap should build")
    .0;

    assert_eq!(bootstrap["sessionId"], json!(session_id.as_str()));
    assert_eq!(bootstrap["scope"], json!("personal"));
    assert!(bootstrap["workspaceId"].is_null());
    assert!(bootstrap["workspacePath"].is_null());
}

#[tokio::test]
async fn settings_bootstrap_requires_explicit_session_scope() {
    let result = settings_bootstrap(State(test_state()), Query(HashMap::new())).await;

    match result {
        Err(ApiError::InvalidInput(message)) => assert_eq!(message, "scope 不能为空"),
        other => panic!("expected missing scope to be rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn settings_scope_rejects_fields_outside_schema() {
    let bootstrap_result = settings_bootstrap(
        State(test_state()),
        Query(HashMap::from([(
            "session_id".to_string(),
            "session-old".to_string(),
        )])),
    )
    .await;
    match bootstrap_result {
        Err(ApiError::InvalidInput(message)) => {
            assert!(message.contains("session_id"), "{message}");
        }
        other => panic!("expected unknown query field error, got {other:?}"),
    }

    let session_save_result = save_orchestrator_session_config(
        State(test_state()),
        Json(json!({ "scope": "personal", "session_id": "session-old", "config": {} })),
    )
    .await;
    match session_save_result {
        Err(ApiError::InvalidRequestBody(message)) => {
            assert!(message.contains("session_id"), "{message}");
        }
        other => panic!("expected unknown body field error, got {other:?}"),
    }
}

#[tokio::test]
async fn settings_bootstrap_rejects_workspace_mismatched_session_scope() {
    let state = test_state();
    let workspace_a = WorkspaceId::new("workspace-settings-a");
    let workspace_b = WorkspaceId::new("workspace-settings-b");
    state
        .workspace_registry
        .register(
            workspace_a.clone(),
            AbsolutePath::new("/tmp/magi-settings-a"),
        )
        .expect("workspace A should register");
    state
        .workspace_registry
        .register(
            workspace_b.clone(),
            AbsolutePath::new("/tmp/magi-settings-b"),
        )
        .expect("workspace B should register");
    let session_b = SessionId::new("session-settings-b");
    state
        .session_store
        .create_session_for_workspace(session_b.clone(), "B 会话", Some(workspace_b.to_string()))
        .expect("session should create");

    let result = settings_bootstrap(
        State(state),
        Query(HashMap::from([
            ("scope".to_string(), "workspace".to_string()),
            ("workspaceId".to_string(), workspace_a.to_string()),
            ("sessionId".to_string(), session_b.to_string()),
        ])),
    )
    .await;

    match result {
        Err(ApiError::InvalidInput(message)) => {
            assert!(
                message.contains("不属于 workspace"),
                "settings bootstrap should reject mismatched scope: {message}"
            );
        }
        other => panic!("unexpected settings bootstrap result: {:?}", other),
    }
}

#[tokio::test]
async fn settings_bootstrap_ignores_persisted_public_alias_sections() {
    let state = test_state();
    state
        .settings_store
        .set_section(
            "orchestrator",
            json!({
                "baseUrl": "https://api.current.example/v1",
                "apiKey": "sk-current",
                "apiProtocol": "openai_chat"
            }),
        )
        .unwrap();
    state
        .settings_store
        .set_section(
            "orchestratorConfig",
            json!({
                "baseUrl": "https://api.alias.example/v1",
                "apiKey": "sk-alias",
                "model": "alias-main",
            }),
        )
        .unwrap();
    state
        .settings_store
        .set_section(
            "workerConfigs",
            json!({
                "alias-worker": {
                    "baseUrl": "https://api.alias.example/v1",
                    "model": "alias-worker"
                }
            }),
        )
        .unwrap();
    state
        .settings_store
        .set_section(
            "auxiliaryConfig",
            json!({
                "baseUrl": "https://api.alias.example/v1",
                "model": "alias-aux"
            }),
        )
        .unwrap();

    let bootstrap = settings_bootstrap(
        State(state),
        Query(HashMap::from([(
            "scope".to_string(),
            "personal".to_string(),
        )])),
    )
    .await
    .expect("settings bootstrap should build")
    .0;

    assert_eq!(
        bootstrap["orchestratorConfig"]["baseUrl"],
        json!("https://api.current.example/v1")
    );
    assert!(
        bootstrap["orchestratorConfig"].get("model").is_none(),
        "全局 orchestratorConfig 只暴露连接配置，不暴露会话主模型"
    );
    assert!(
        bootstrap["workerConfigs"]
            .as_object()
            .expect("workerConfigs should be an object")
            .is_empty(),
        "public alias workerConfigs must not be treated as persisted worker settings"
    );
    assert!(
        bootstrap["auxiliaryConfig"]
            .as_object()
            .expect("auxiliaryConfig should be an object")
            .is_empty(),
        "public alias auxiliaryConfig must not be treated as persisted auxiliary settings"
    );
}

#[tokio::test]
async fn settings_bootstrap_aligns_registry_engine_llm_from_worker_config() {
    let state = test_state();
    state
        .settings_store
        .set_section(
            "workers",
            json!({
                "sonnet-worker": {
                    "apiProtocol": "anthropic_messages",
                    "baseUrl": "https://api.anthropic.com",
                    "model": "claude-worker-test",
                }
            }),
        )
        .unwrap();
    state
        .settings_store
        .set_section(
            "engines",
            json!([{
                "id": "sonnet-worker",
                "displayName": "Sonnet Worker",
                "llm": {
                    "apiProtocol": "openai_chat",
                    "baseUrl": "https://api.openai.com",
                    "model": "stale-openai-model",
                }
            }]),
        )
        .unwrap();

    let bootstrap = settings_bootstrap(
        State(state.clone()),
        Query(HashMap::from([(
            "scope".to_string(),
            "personal".to_string(),
        )])),
    )
    .await
    .expect("settings bootstrap should build")
    .0;

    assert_eq!(
        bootstrap["registryEngines"][0]["llm"]["model"],
        json!("claude-worker-test")
    );
    assert_eq!(
        bootstrap["registryEngines"][0]["llm"]["apiProtocol"],
        json!("anthropic_messages")
    );
    let persisted_engines = state.settings_store.get_section("engines");
    assert_eq!(
        persisted_engines[0]["llm"]["model"],
        json!("stale-openai-model")
    );
}

#[tokio::test]
async fn role_upsert_lists_user_role_and_persists_markdown() {
    let directory = tempfile::tempdir().expect("角色目录应创建");
    let state = role_test_state(directory.path());

    let response = upsert_role(State(state.clone()), Json(role_payload("data-analyst")))
        .await
        .expect("角色创建应成功")
        .0;
    assert_eq!(response["role"]["templateId"], json!("data-analyst"));
    assert_eq!(response["role"]["source"], json!("user"));
    assert_eq!(response["role"]["editable"], json!(true));
    assert_eq!(response["role"]["roleRevision"], json!(1));
    assert!(state.agent_role_registry.is_user_defined("data-analyst"));
    assert!(directory.path().join("data-analyst.md").is_file());

    let templates = list_role_templates(State(state), Query(HashMap::new()))
        .await
        .0;
    assert!(
        templates["templates"]
            .as_array()
            .expect("templates 应为数组")
            .iter()
            .any(|template| template["templateId"] == json!("data-analyst"))
    );
}

#[tokio::test]
async fn role_edit_requires_current_revision_and_increments_it() {
    let directory = tempfile::tempdir().expect("角色目录应创建");
    let state = role_test_state(directory.path());
    let _ = upsert_role(State(state.clone()), Json(role_payload("revision-role")))
        .await
        .expect("角色创建应成功");

    let mut update = role_payload("revision-role");
    update["role"]["description"] = json!("第一次编辑");
    update["expectedRoleRevision"] = json!(1);
    let response = upsert_role(State(state.clone()), Json(update))
        .await
        .expect("角色编辑应成功")
        .0;
    assert_eq!(response["role"]["roleRevision"], json!(2));

    let mut stale = role_payload("revision-role");
    stale["role"]["description"] = json!("过期编辑");
    stale["expectedRoleRevision"] = json!(1);
    assert!(matches!(
        upsert_role(State(state.clone()), Json(stale)).await,
        Err(ApiError::Conflict(_))
    ));
    assert_eq!(
        state
            .agent_role_registry
            .get("revision-role")
            .unwrap()
            .description,
        "第一次编辑"
    );
}

#[tokio::test]
async fn role_export_import_round_trip_supports_conflict_strategies() {
    let directory = tempfile::tempdir().expect("角色目录应创建");
    let state = role_test_state(directory.path());
    let _ = upsert_role(State(state.clone()), Json(role_payload("round-trip")))
        .await
        .expect("角色创建应成功");

    let exported = export_role(
        State(state.clone()),
        Query(HashMap::from([(
            String::from("templateId"),
            String::from("round-trip"),
        )])),
    )
    .await
    .expect("角色导出应成功")
    .0;
    let content = exported["content"].as_str().expect("导出内容应为字符串");
    assert!(!content.contains("role_revision:"));
    assert!(!content.contains("api_key"));
    assert!(!content.contains("workspace"));

    let _ = delete_role(
        State(state.clone()),
        Json(json!({"templateId": "round-trip", "expectedRoleRevision": 1})),
    )
    .await
    .expect("角色删除应成功");
    assert!(!state.agent_role_registry.contains("round-trip"));

    let _ = import_role(
        State(state.clone()),
        Json(json!({"content": content, "conflict": "reject"})),
    )
    .await
    .expect("角色重新导入应成功");
    assert_eq!(
        state
            .agent_role_registry
            .get("round-trip")
            .unwrap()
            .system_prompt,
        "你是数据分析师，先核对数据契约，再输出可复现结论。"
    );

    let reject = import_role(
        State(state.clone()),
        Json(json!({"content": content, "conflict": "reject"})),
    )
    .await;
    assert!(matches!(
        reject,
        Err(ApiError::Conflict(message)) if message.contains("已存在")
    ));

    let overwrite = import_role(
        State(state.clone()),
        Json(json!({"content": content, "conflict": "overwrite"})),
    )
    .await
    .expect("覆盖导入应成功");
    assert_eq!(overwrite.0["role"]["templateId"], json!("round-trip"));

    let renamed = import_role(
        State(state.clone()),
        Json(json!({
            "content": content,
            "conflict": "rename",
            "newId": "round-trip-copy"
        })),
    )
    .await
    .expect("另存为导入应成功");
    assert_eq!(renamed.0["role"]["templateId"], json!("round-trip-copy"));
    assert!(state.agent_role_registry.contains("round-trip-copy"));

    let auto_renamed = import_role(
        State(state.clone()),
        Json(json!({
            "content": content,
            "conflict": "rename"
        })),
    )
    .await
    .expect("未指定 newId 时应自动另存为");
    assert_eq!(
        auto_renamed.0["role"]["templateId"],
        json!("round-trip-copy-2")
    );
    assert!(state.agent_role_registry.contains("round-trip-copy-2"));
}

#[tokio::test]
async fn role_import_overwrite_requires_existing_user_role() {
    let directory = tempfile::tempdir().expect("角色目录应创建");
    let state = role_test_state(directory.path());
    let content = role_export_content(&state, "executor").replacen(
        "id: executor",
        "id: missing-overwrite-target",
        1,
    );

    let missing = import_role(
        State(state.clone()),
        Json(json!({
            "content": content,
            "conflict": "overwrite",
            "newId": "missing-overwrite-target"
        })),
    )
    .await;
    assert!(matches!(
        missing,
        Err(ApiError::Conflict(message)) if message.contains("不存在") && message.contains("覆盖导入")
    ));
    assert!(
        !state
            .agent_role_registry
            .contains("missing-overwrite-target")
    );

    let builtin = import_role(
        State(state.clone()),
        Json(json!({
            "content": role_export_content(&state, "executor"),
            "conflict": "overwrite"
        })),
    )
    .await;
    assert!(matches!(
        builtin,
        Err(ApiError::Conflict(message)) if message.contains("系统内置") && message.contains("覆盖")
    ));
}

#[tokio::test]
async fn role_delete_removes_binding_and_stale_revision_preserves_it() {
    let directory = tempfile::tempdir().expect("角色目录应创建");
    let state = role_test_state(directory.path());
    let _ = upsert_role(State(state.clone()), Json(role_payload("bound-role")))
        .await
        .expect("角色创建应成功");
    state
        .settings_store
        .set_section(
            "engines",
            json!([{"id": "engine-a", "displayName": "引擎 A", "llm": {}}]),
        )
        .expect("引擎配置应写入");
    let _ = upsert_agent(
        State(state.clone()),
        Json(json!({"templateId": "bound-role", "engineId": "engine-a"})),
    )
    .await
    .expect("角色绑定应成功");
    assert!(
        resolve_registry_agents(&state)
            .iter()
            .any(|agent| agent["templateId"] == json!("bound-role")
                && agent["engineId"] == json!("engine-a"))
    );

    let stale = delete_role(
        State(state.clone()),
        Json(json!({"templateId": "bound-role", "expectedRoleRevision": 99})),
    )
    .await;
    assert!(matches!(stale, Err(ApiError::Conflict(_))));
    assert!(state.agent_role_registry.contains("bound-role"));
    assert!(
        resolve_registry_agents(&state)
            .iter()
            .any(|agent| agent["templateId"] == json!("bound-role")
                && agent["engineId"] == json!("engine-a"))
    );

    let _ = delete_role(
        State(state.clone()),
        Json(json!({"templateId": "bound-role", "expectedRoleRevision": 1})),
    )
    .await
    .expect("角色删除应成功");
    assert!(!state.agent_role_registry.contains("bound-role"));
    assert!(
        !resolve_registry_agents(&state)
            .iter()
            .any(|agent| agent["templateId"] == json!("bound-role"))
    );
}

#[tokio::test]
async fn role_mutations_reject_builtin_and_invalid_definitions() {
    let directory = tempfile::tempdir().expect("角色目录应创建");
    let state = role_test_state(directory.path());

    let mut builtin = role_payload("executor");
    builtin["role"]["displayName"] = json!("覆盖内置");
    assert!(matches!(
        upsert_role(State(state.clone()), Json(builtin)).await,
        Err(ApiError::Conflict(_))
    ));
    assert!(matches!(
        delete_role(
            State(state.clone()),
            Json(json!({"templateId": "executor", "expectedRoleRevision": 1}))
        )
        .await,
        Err(ApiError::Conflict(_))
    ));

    let mut unknown_capability = role_payload("bad-capability");
    unknown_capability["role"]["capabilities"] = json!(["does-not-exist"]);
    assert!(matches!(
        upsert_role(State(state.clone()), Json(unknown_capability)).await,
        Err(ApiError::InvalidInput(message)) if message.contains("专业能力不存在")
    ));

    let mut coordinator = role_payload("bad-coordinator");
    coordinator["role"]["coordinatorMode"] = json!(true);
    assert!(matches!(
        upsert_role(State(state.clone()), Json(coordinator)).await,
        Err(ApiError::InvalidInput(message)) if message.contains("coordinator_mode")
    ));

    let mut missing_local_agent = role_payload("bad-kind");
    missing_local_agent["role"]["supportedKinds"] = json!(["remote_agent"]);
    assert!(matches!(
        upsert_role(State(state.clone()), Json(missing_local_agent)).await,
        Err(ApiError::InvalidInput(message)) if message.contains("local_agent")
    ));

    let mut malformed_number = role_payload("bad-number");
    malformed_number["role"]["parallelismLimit"] = json!(-1);
    assert!(matches!(
        upsert_role(State(state), Json(malformed_number)).await,
        Err(ApiError::InvalidInput(message)) if message.contains("parallelismLimit")
    ));
}

#[tokio::test]
async fn role_delete_rejects_active_worker_lease_and_preserves_role_and_binding() {
    let directory = tempfile::tempdir().expect("角色目录应创建");
    let task_store = Arc::new(magi_orchestrator::task_store::TaskStore::new());
    let state = role_test_state(directory.path()).with_task_store(Arc::clone(&task_store));
    let _ = upsert_role(State(state.clone()), Json(role_payload("active-role")))
        .await
        .expect("角色创建应成功");
    state
        .settings_store
        .set_section(
            "agents",
            json!([{"templateId": "active-role", "engineId": "engine-a"}]),
        )
        .expect("绑定应写入");

    let now = magi_core::UtcMillis::now();
    let root_task_id = magi_core::TaskId::new("root-active-role");
    let task_id = magi_core::TaskId::new("task-active-role");
    let mission_id = magi_core::MissionId::new("mission-active-role");
    task_store
        .insert_task(magi_core::Task {
            task_id: root_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: magi_core::TaskKind::LocalAgent,
            title: "root".to_string(),
            goal: "root".to_string(),
            status: magi_core::TaskStatus::Running,
            dependency_ids: Vec::new(),
            required_children: vec![task_id.clone()],
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        })
        .expect("根任务应写入");
    task_store
        .insert_task(magi_core::Task {
            task_id: task_id.clone(),
            mission_id,
            root_task_id: root_task_id.clone(),
            parent_task_id: Some(root_task_id.clone()),
            kind: magi_core::TaskKind::LocalAgent,
            title: "active worker".to_string(),
            goal: "active worker".to_string(),
            status: magi_core::TaskStatus::Pending,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: Some(magi_core::TaskExecutorBinding::for_role("active-role")),
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        })
        .expect("子任务应写入");
    task_store
        .grant_lease_and_start_task(
            &task_id,
            &root_task_id,
            &magi_core::WorkerId::new("worker-active-role"),
            "active-role",
            60_000,
        )
        .expect("租约应授予")
        .expect("任务应获得活跃租约");

    let result = delete_role(
        State(state.clone()),
        Json(json!({"templateId": "active-role", "expectedRoleRevision": 1})),
    )
    .await;
    assert!(
        matches!(result, Err(ApiError::Conflict(message)) if message.contains("运行中的 Worker"))
    );
    assert!(state.agent_role_registry.contains("active-role"));
    assert_eq!(
        state.settings_store.get_section("agents"),
        json!([{"templateId": "active-role", "engineId": "engine-a"}])
    );
}

#[tokio::test]
async fn role_import_rejects_oversized_and_unknown_conflict_requests() {
    let directory = tempfile::tempdir().expect("角色目录应创建");
    let state = role_test_state(directory.path());
    let oversized = "x".repeat(100_001);
    assert!(matches!(
        import_role(
            State(state.clone()),
            Json(json!({"content": oversized, "conflict": "reject"}))
        )
        .await,
        Err(ApiError::InvalidInput(message)) if message.contains("100000")
    ));
    assert!(matches!(
        import_role(
            State(state),
            Json(json!({"content": "---\nid: bad\n---\n提示词", "conflict": "merge"}))
        )
        .await,
        Err(ApiError::InvalidInput(message)) if message.contains("reject")
    ));
}

#[tokio::test]
async fn role_mutations_reject_malformed_revision_and_conflict_fields() {
    let directory = tempfile::tempdir().expect("角色目录应创建");
    let state = role_test_state(directory.path());

    let mut role = role_payload("strict-input");
    role["expectedRoleRevision"] = json!("1");
    assert!(matches!(
        upsert_role(State(state.clone()), Json(role)).await,
        Err(ApiError::InvalidInput(message)) if message.contains("expectedRoleRevision")
    ));

    let mut role = role_payload("strict-input");
    role["expectedRoleRevision"] = json!(-1);
    assert!(matches!(
        upsert_role(State(state.clone()), Json(role)).await,
        Err(ApiError::InvalidInput(message)) if message.contains("expectedRoleRevision")
    ));

    assert!(matches!(
        import_role(
            State(state.clone()),
            Json(json!({
                "content": "---\nid: strict-input\nsupported_kinds: [local_agent]\ncapabilities: [general_engineering]\n---\n提示词",
                "conflict": 1
            }))
        )
        .await,
        Err(ApiError::InvalidInput(message)) if message.contains("conflict")
    ));

    assert!(matches!(
        import_role(
            State(state),
            Json(json!({
                "content": "---\nid: strict-input\nsupported_kinds: [local_agent]\ncapabilities: [general_engineering]\n---\n提示词",
                "conflict": "rename",
                "newId": 7
            }))
        )
        .await,
        Err(ApiError::InvalidInput(message)) if message.contains("newId")
    ));
}

#[tokio::test]
async fn settings_bootstrap_filters_mcp_servers_without_id() {
    let state = test_state();
    state
        .settings_store
        .set_section(
            "mcpServers",
            json!([
                { "name": "broken", "command": "npx", "enabled": false },
                { "server": { "id": "wrapped-server", "command": "npx", "enabled": false } },
                { "id": "valid-server", "command": "npx", "enabled": false },
                "invalid-entry"
            ]),
        )
        .unwrap();

    let bootstrap = settings_bootstrap(
        State(state),
        Query(HashMap::from([(
            "scope".to_string(),
            "personal".to_string(),
        )])),
    )
    .await
    .expect("settings bootstrap should build")
    .0;
    let servers = bootstrap["mcpServers"]
        .as_array()
        .expect("mcpServers should be an array");
    assert_eq!(servers.len(), 1);
    assert!(
        servers
            .iter()
            .all(|server| server["id"].as_str().is_some_and(|id| !id.is_empty())),
        "bootstrap must not expose MCP server entries without id"
    );
    assert!(
        servers
            .iter()
            .any(|server| { server["id"] == json!("valid-server") })
    );
    assert!(
        servers.iter().all(|server| {
            server["enabled"] == json!(false)
                && server["connected"] == json!(false)
                && server["health"] == json!("disabled")
                && server.get("error").is_none()
        }),
        "disabled MCP servers should not be exposed as connection failures"
    );
}

#[tokio::test]
async fn settings_bootstrap_exposes_mcp_env_values_for_editing() {
    let state = test_state();
    state
        .settings_store
        .set_section(
            "mcpServers",
            json!([
                {
                    "id": "secret-server",
                    "command": "npx",
                    "enabled": false,
                    "workspaceId": "workspace-old",
                    "workspacePath": "/tmp/old",
                    "sessionId": "session-old",
                    "env": {
                        "TOKEN": "secret-token"
                    }
                }
            ]),
        )
        .unwrap();

    let bootstrap = settings_bootstrap(
        State(state),
        Query(HashMap::from([(
            "scope".to_string(),
            "personal".to_string(),
        )])),
    )
    .await
    .expect("settings bootstrap should build")
    .0;

    assert_eq!(
        bootstrap["mcpServers"][0]["env"]["TOKEN"],
        json!("secret-token")
    );
    for key in ["workspaceId", "workspacePath", "sessionId"] {
        assert!(
            bootstrap["mcpServers"][0].get(key).is_none(),
            "settings bootstrap must not expose stale MCP scope field {key}"
        );
    }
}

#[tokio::test]
async fn settings_bootstrap_does_not_probe_unconnected_mcp_servers() {
    let state = test_state();
    state
        .settings_store
        .set_section(
            "mcpServers",
            json!([
                {
                    "id": "slow-or-missing-server",
                    "command": "definitely-not-existing-magi-mcp-command",
                    "enabled": true
                }
            ]),
        )
        .unwrap();

    let bootstrap = settings_bootstrap(
        State(state),
        Query(HashMap::from([(
            "scope".to_string(),
            "personal".to_string(),
        )])),
    )
    .await
    .expect("settings bootstrap should build")
    .0;
    let server = &bootstrap["mcpServers"][0];

    assert_eq!(server["connected"], json!(false));
    assert_eq!(server["health"], json!("disconnected"));
    assert!(
        server.get("error").is_none(),
        "settings bootstrap must not synchronously probe MCP process health"
    );
}

#[tokio::test]
async fn settings_bootstrap_full_scope_hydrates_external_capability_dependencies() {
    let state = test_state_with_external_catalog_snapshot(Some(ExternalToolCatalogSnapshot {
        instruction_skill_count: 0,
        skill_tools: Vec::new(),
        mcp_servers: vec![ExternalMcpServerCatalogEntry {
            server_id: "loopback-mcp".to_string(),
            name: "loopback-mcp".to_string(),
            enabled: true,
            connected: false,
            health: "disconnected".to_string(),
            tool_count: Some(7),
            error: None,
        }],
        mcp_tools: Vec::new(),
    }));
    state
        .settings_store
        .set_section(
            "mcpServers",
            json!([
                {
                    "id": "loopback-mcp",
                    "name": "loopback-mcp",
                    "command": "npx",
                    "enabled": true
                }
            ]),
        )
        .unwrap();

    let bootstrap = settings_bootstrap(
        State(state),
        Query(HashMap::from([(
            "scope".to_string(),
            "personal".to_string(),
        )])),
    )
    .await
    .expect("settings bootstrap should build")
    .0;

    assert_eq!(bootstrap["bootstrapScope"], json!("full"));
    assert_eq!(bootstrap["mcpServersHydrated"], json!(true));
    assert_eq!(bootstrap["mcpServers"][0]["connected"], json!(false));
    assert_eq!(bootstrap["mcpServers"][0]["health"], json!("disconnected"));

    let dependencies = bootstrap["capabilityDependencies"]
        .as_array()
        .expect("capability dependencies should be an array");
    let mcp_dependency = dependencies
        .iter()
        .find(|dependency| dependency["name"] == json!("mcp_servers"))
        .expect("mcp dependency should be listed");
    assert_eq!(mcp_dependency["status"], json!("not_ready"));
    assert_eq!(mcp_dependency["configuredCount"], json!(1));
    assert_eq!(mcp_dependency["enabledCount"], json!(1));
    assert_eq!(mcp_dependency["readyCount"], json!(0));
    assert_eq!(mcp_dependency["enabledToolCount"], json!(7));
    assert_eq!(mcp_dependency["readyToolCount"], json!(0));
    assert_eq!(
        mcp_dependency["toolCount"],
        json!(0),
        "toolCount must remain the currently usable MCP tool count"
    );
}

#[tokio::test]
async fn settings_bootstrap_core_scope_defers_mcp_hydration() {
    let state = test_state();
    let bootstrap = settings_bootstrap(
        State(state),
        Query(HashMap::from([
            ("scope".to_string(), "personal".to_string()),
            ("bootstrapScope".to_string(), "core".to_string()),
        ])),
    )
    .await
    .expect("settings bootstrap should build")
    .0;

    assert_eq!(bootstrap["bootstrapScope"], json!("core"));
    assert_eq!(bootstrap["mcpServersHydrated"], json!(false));
    let dependencies = bootstrap["capabilityDependencies"]
        .as_array()
        .expect("capability dependencies should be an array");
    let mcp_dependency = dependencies
        .iter()
        .find(|dependency| dependency["name"] == json!("mcp_servers"))
        .expect("mcp dependency should be listed");
    assert_eq!(
        mcp_dependency["status"],
        json!("disabled"),
        "core settings bootstrap should not hydrate MCP diagnostics"
    );
}

#[test]
fn scoped_settings_section_request_strips_all_scope_binding_fields() {
    let cleaned = scoped_settings_section_request(&json!({
        "model": "gpt-test",
        "workspaceId": "workspace-a",
        "workspacePath": "/tmp/a",
        "sessionId": "session-a"
    }))
    .expect("config wrapper should be accepted");

    assert_eq!(cleaned["model"], json!("gpt-test"));
    for key in ["workspaceId", "workspacePath", "sessionId"] {
        assert!(
            cleaned.get(key).is_none(),
            "{key} should not be persisted in settings sections"
        );
    }
}

#[test]
fn model_settings_section_request_rejects_unknown_fields() {
    let error = model_settings_section_request(&json!({
        "unexpected": "openai",
        "baseUrl": "https://api.example.com/v1",
        "apiKey": "sk-test",
        "model": "gpt-test"
    }))
    .expect_err("模型配置保存入口必须拒绝 schema 之外的字段");

    match error {
        ApiError::InvalidInput(message) => {
            assert!(message.contains("unexpected"));
        }
        other => panic!("expected invalid input, got {other:?}"),
    }
}

#[tokio::test]
async fn save_orchestrator_config_writes_only_the_source_catalog() {
    let state = test_state();
    let response = save_orchestrator_config(
        State(state.clone()),
        Json(json!({
            "expectedRevision": 0,
            "sources": [main_source("one", &["m1"])],
            "sessionId": "session-a",
            "workspaceId": "workspace-a"
        })),
    )
    .await
    .expect("source catalog should save")
    .0;

    let saved = state.settings_store.get_section("orchestrator");
    assert_eq!(saved["revision"], json!(1));
    assert_eq!(saved["sources"][0]["id"], json!("one"));
    assert_eq!(saved["sources"][0]["apiKey"], json!("sk-one"));
    assert!(saved.get("sessionId").is_none());
    assert!(saved.get("workspaceId").is_none());
    assert_eq!(response["config"], saved);
    assert!(
        state
            .settings_store
            .get_session_section(&SessionId::new("session-a"), "orchestrator")
            .is_null(),
        "数据源保存接口不得写会话模型覆盖"
    );

    let stale = save_orchestrator_config(
        State(state.clone()),
        Json(json!({"expectedRevision": 0, "sources": []})),
    )
    .await
    .expect_err("过期修订号必须被拒绝");
    assert!(matches!(stale, ApiError::InvalidInput(_)));
    assert_eq!(
        state.settings_store.get_section("orchestrator")["revision"],
        json!(1)
    );

    for invalid in [
        json!({"expectedRevision": 1, "sources": [main_source("one", &["m"]), main_source("one", &["m"])]}),
        json!({"expectedRevision": 1, "baseUrl": "https://old.example/v1", "apiKey": "k", "apiProtocol": "openai_chat"}),
        json!({"sources": []}),
    ] {
        let error = save_orchestrator_config(State(state.clone()), Json(invalid))
            .await
            .expect_err("无效数据源配置必须被拒绝");
        assert!(matches!(error, ApiError::InvalidInput(_)));
    }
}

#[tokio::test]
async fn save_image_generation_config_persists_canonical_model_fields() {
    let state = test_state();
    let response = save_image_generation_config(
        State(state.clone()),
        Json(json!({
            "baseUrl": "https://cpa.example.com/v1",
            "apiKey": "sk-image",
            "model": "gpt-image-1",
            "apiProtocol": "openai_chat",
            "reasoningEffort": "high",
            "workspaceId": "workspace-ignored"
        })),
    )
    .await
    .expect("image generation config should save")
    .0;

    let saved = state.settings_store.get_section("imageGeneration");
    assert_eq!(saved["baseUrl"], json!("https://cpa.example.com/v1"));
    assert_eq!(saved["apiKey"], json!("sk-image"));
    assert_eq!(saved["model"], json!("gpt-image-1"));
    assert!(saved.get("reasoningEffort").is_none());
    assert!(saved.get("workspaceId").is_none());
    assert_eq!(response["config"], saved);
}

#[tokio::test]
async fn worker_and_auxiliary_save_return_persisted_config() {
    let state = test_state();
    let worker_response = save_worker_config(
        State(state.clone()),
        Json(json!({
            "worker": "test-worker",
            "config": {
                "baseUrl": "https://api.example.com/v1",
                "apiKey": "sk-worker",
                "model": "worker-model",
                "apiProtocol": "openai_chat",
                "reasoningEffort": "high"
            }
        })),
    )
    .await
    .expect("worker config should save")
    .0;
    assert_eq!(worker_response["worker"], json!("test-worker"));
    assert_eq!(
        worker_response["config"],
        state.settings_store.get_section("workers")["test-worker"]
    );

    let auxiliary_response = save_auxiliary_config(
        State(state.clone()),
        Json(json!({
            "baseUrl": "https://api.example.com/v1",
            "apiKey": "sk-auxiliary",
            "model": "auxiliary-model",
            "apiProtocol": "openai_responses"
        })),
    )
    .await
    .expect("auxiliary config should save")
    .0;
    assert_eq!(
        auxiliary_response["config"],
        state.settings_store.get_section("auxiliary")
    );
}

#[tokio::test]
async fn vision_config_save_persists_context_window_and_text_model_rules_atomically() {
    let state = test_state();
    let saved = save_vision_config(
        State(state.clone()),
        Json(json!({
            "baseUrl": "https://vision.example.com/v1",
            "apiKey": "sk-vision",
            "model": "vision-capable-model",
            "apiProtocol": "openai_chat",
            "contextWindowTokens": 256000,
            "textModelRules": [
                {"matchMode": "exact", "pattern": "company-text-model"},
                {"matchMode": "regex", "pattern": "^legacy-[0-9]+$"}
            ]
        })),
    )
    .await
    .expect("vision config should save")
    .0;
    assert_eq!(
        saved["config"],
        state.settings_store.get_section(VISION_MODEL_SECTION)
    );
    assert_eq!(
        saved["modelContextWindows"]["vision-capable-model"],
        json!(256000)
    );
    assert_eq!(
        state
            .settings_store
            .get_section(MODEL_CONTEXT_WINDOWS_SECTION)["vision-capable-model"],
        json!(256000)
    );
}

#[tokio::test]
async fn vision_config_save_rejects_invalid_rules_without_partial_write() {
    let state = test_state();
    state
        .settings_store
        .set_section(
            VISION_MODEL_SECTION,
            json!({
                "baseUrl": "https://old-vision.example.com/v1",
                "apiKey": "old-key",
                "model": "old-vision-model",
                "apiProtocol": "openai_chat",
                "contextWindowTokens": 128000,
                "textModelRules": []
            }),
        )
        .unwrap();
    state
        .settings_store
        .set_section(
            MODEL_CONTEXT_WINDOWS_SECTION,
            json!({"old-vision-model": 128000}),
        )
        .unwrap();
    let original_vision = state.settings_store.get_section(VISION_MODEL_SECTION);
    let original_windows = state
        .settings_store
        .get_section(MODEL_CONTEXT_WINDOWS_SECTION);

    let result = save_vision_config(
        State(state.clone()),
        Json(json!({
            "baseUrl": "https://new-vision.example.com/v1",
            "apiKey": "new-key",
            "model": "new-vision-model",
            "apiProtocol": "openai_chat",
            "contextWindowTokens": 256000,
            "textModelRules": [{"matchMode": "regex", "pattern": "("}]
        })),
    )
    .await;

    assert!(matches!(result, Err(ApiError::InvalidInput(_))));
    assert_eq!(
        state.settings_store.get_section(VISION_MODEL_SECTION),
        original_vision
    );
    assert_eq!(
        state
            .settings_store
            .get_section(MODEL_CONTEXT_WINDOWS_SECTION),
        original_windows
    );
}

#[tokio::test]
async fn save_orchestrator_session_config_writes_only_session_override() {
    let state = test_state();
    let _workspace = register_test_workspace(&state, "workspace-session-model");
    let session_id = SessionId::new("session-model-override");
    seed_session(
        &state,
        test_session_record(&session_id, "workspace-session-model", "会话模型覆盖"),
    );
    seed_main_sources(
        &state,
        vec![main_source(
            "one",
            &[
                "session-main-model",
                "personal-main-model",
                "model-before",
                "model-after",
            ],
        )],
    );

    let response = save_orchestrator_session_config(
        State(state.clone()),
        Json(json!({
            "sessionId": session_id.as_str(),
            "scope": "workspace",
            "workspaceId": "workspace-session-model",
            "config": {
                "sourceId": "custom/one",
                "model": "session-main-model",
                "reasoningEffort": "high"
            }
        })),
    )
    .await
    .expect("session orchestrator override should save")
    .0;

    assert_eq!(response["saved"], json!(true));
    let global = state.settings_store.get_section("orchestrator");
    assert_eq!(global["sources"][0]["apiKey"], json!("sk-one"));
    let saved = state
        .settings_store
        .get_session_section(&session_id, "orchestrator");
    assert_eq!(saved["sourceId"], json!("custom/one"));
    assert_eq!(saved["model"], json!("session-main-model"));
    assert_eq!(saved["reasoningEffort"], json!("high"));
    assert!(
        saved.get("baseUrl").is_none() && saved.get("apiKey").is_none(),
        "会话覆盖不得持久化连接凭据"
    );
    assert_eq!(
        response["effectiveOrchestratorConfig"]["baseUrl"],
        json!("https://one.example.test/v1")
    );
    assert_eq!(
        response["effectiveOrchestratorConfig"]["model"],
        json!("session-main-model")
    );
    let configuration_event = state
        .event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .find(|event| event.event_type == "session.configuration.updated")
        .expect("会话模型配置变更必须广播给其他窗口");
    assert_eq!(configuration_event.session_id.as_ref(), Some(&session_id));
    assert_eq!(
        configuration_event
            .workspace_id
            .as_ref()
            .map(ToString::to_string),
        Some("workspace-session-model".to_string())
    );
    assert_eq!(
        configuration_event.payload["orchestrator_session_config"]["model"],
        json!("session-main-model")
    );
    assert_eq!(
        state
            .settings_store
            .get_section(ORCHESTRATOR_SESSION_DEFAULTS_SECTION),
        json!({
            "sourceId": "custom/one",
            "model": "session-main-model",
            "reasoningEffort": "high"
        }),
        "当前会话的最新模型选择必须原子更新为后续新会话默认值"
    );
}

#[tokio::test]
async fn save_orchestrator_session_config_supports_personal_session_scope() {
    let state = test_state();
    let session_id = SessionId::new("session-personal-model-override");
    state
        .session_store
        .create_session(session_id.clone(), "个人模型配置")
        .expect("personal session should be creatable");
    seed_main_sources(
        &state,
        vec![main_source(
            "one",
            &[
                "session-main-model",
                "personal-main-model",
                "model-before",
                "model-after",
            ],
        )],
    );

    let response = save_orchestrator_session_config(
        State(state.clone()),
        Json(json!({
            "sessionId": session_id.as_str(),
            "scope": "personal",
            "config": {
                "sourceId": "custom/one",
                "model": "personal-main-model",
                "reasoningEffort": "high"
            }
        })),
    )
    .await
    .expect("personal session orchestrator override should save")
    .0;

    assert_eq!(response["saved"], json!(true));
    assert!(response["workspaceId"].is_null());
    assert_eq!(
        response["orchestratorSessionConfig"]["model"],
        json!("personal-main-model")
    );
    let event = state
        .event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .find(|event| event.event_type == "session.configuration.updated")
        .expect("personal session configuration should publish");
    assert_eq!(event.session_id.as_ref(), Some(&session_id));
    assert!(event.workspace_id.is_none());
    assert!(event.payload["workspace_id"].is_null());
}

#[tokio::test]
async fn save_orchestrator_session_config_merges_independent_model_fields() {
    let state = test_state();
    let _workspace = register_test_workspace(&state, "workspace-session-model-merge");
    let session_id = SessionId::new("session-model-merge");
    seed_session(
        &state,
        test_session_record(
            &session_id,
            "workspace-session-model-merge",
            "会话模型字段独立保存",
        ),
    );
    seed_main_sources(
        &state,
        vec![main_source(
            "one",
            &[
                "session-main-model",
                "personal-main-model",
                "model-before",
                "model-after",
            ],
        )],
    );
    state
        .settings_store
        .set_session_section(
            &session_id,
            "orchestrator",
            json!({
                "sourceId": "custom/one",
                "model": "model-before",
                "reasoningEffort": "high"
            }),
        )
        .unwrap();

    let response = save_orchestrator_session_config(
        State(state.clone()),
        Json(json!({
            "sessionId": session_id.as_str(),
            "scope": "workspace",
            "workspaceId": "workspace-session-model-merge",
            "config": {
                "model": "model-after"
            }
        })),
    )
    .await
    .expect("model-only session update should save")
    .0;

    let saved = state
        .settings_store
        .get_session_section(&session_id, "orchestrator");
    assert_eq!(saved["model"], json!("model-after"));
    assert_eq!(
        saved["reasoningEffort"],
        json!("high"),
        "单独切换模型不得清空当前会话的推理强度"
    );
    assert_eq!(response["orchestratorSessionConfig"], saved);
    assert_eq!(
        response["effectiveOrchestratorConfig"]["reasoningEffort"],
        json!("high")
    );
    assert!(saved.get("previousModel").is_none());
    assert_eq!(
        state
            .settings_store
            .get_section(ORCHESTRATOR_SESSION_DEFAULTS_SECTION),
        json!({
            "sourceId": "custom/one",
            "model": "model-after",
            "reasoningEffort": "high"
        }),
        "只切换模型时默认值必须保留原会话推理强度"
    );
    assert!(saved.get("modelSwitchPending").is_none());
}

#[test]
fn model_reselection_keeps_only_the_user_selected_model() {
    let state = test_state();
    let session_id = SessionId::new("session-model-reselection");
    seed_main_sources(
        &state,
        vec![main_source(
            "one",
            &["verified-model", "candidate-a", "candidate-b"],
        )],
    );
    state
            .settings_store
            .set_session_section(
                &session_id,
                "orchestrator",
                json!({ "sourceId": "custom/one", "model": "verified-model", "reasoningEffort": "medium" }),
            )
            .unwrap();

    let first = save_orchestrator_session_override_for_session(
        &state,
        &session_id,
        &json!({ "sourceId": "custom/one", "model": "candidate-a" }),
    )
    .unwrap()
    .unwrap();
    assert_eq!(first["model"], json!("candidate-a"));

    let second = save_orchestrator_session_override_for_session(
        &state,
        &session_id,
        &json!({ "sourceId": "custom/one", "model": "candidate-b" }),
    )
    .unwrap()
    .unwrap();
    assert_eq!(second["model"], json!("candidate-b"));
    assert!(second.get("previousModel").is_none());
    assert!(second.get("modelSwitchPending").is_none());
}

#[tokio::test]
async fn model_context_window_save_is_global_and_broadcast() {
    let state = test_state();
    let response = save_model_context_window(
        State(state.clone()),
        Json(json!({
            "model": " GPT-5.6 ",
            "contextWindowTokens": 1_000_000
        })),
    )
    .await
    .expect("model context window should save")
    .0;

    assert_eq!(response["modelContextWindows"]["gpt-5.6"], json!(1_000_000));
    assert_eq!(
        state.settings_store.get_section("modelContextWindows")["gpt-5.6"],
        json!(1_000_000)
    );
    assert!(
        state
            .event_bus
            .snapshot()
            .recent_events
            .iter()
            .any(|event| event.event_type == "model.context_window.updated")
    );
}

#[tokio::test]
async fn model_context_window_save_is_scoped_by_main_source() {
    let state = test_state();
    state
        .settings_store
        .set_section(
            "orchestrator",
            json!({
                "sources": [
                    {
                        "id": "one",
                        "name": "One",
                        "baseUrl": "https://one.example/v1",
                        "apiKey": "one-key",
                        "apiProtocol": "openai_chat",
                        "models": [{"id": "shared-model"}]
                    },
                    {
                        "id": "two",
                        "name": "Two",
                        "baseUrl": "https://two.example/v1",
                        "apiKey": "two-key",
                        "apiProtocol": "openai_chat",
                        "models": [{"id": "shared-model"}]
                    }
                ]
            }),
        )
        .unwrap();

    for (source_id, context_window_tokens) in [("custom/one", 128_000), ("custom/two", 512_000)] {
        let response = save_model_context_window(
            State(state.clone()),
            Json(json!({
                "sourceId": source_id,
                "model": "shared-model",
                "contextWindowTokens": context_window_tokens
            })),
        )
        .await
        .expect("source model context window should save")
        .0;
        assert_eq!(response["sourceId"], json!(source_id));
    }

    let context_windows = state.settings_store.get_section("modelContextWindows");
    assert_eq!(context_windows["custom/one::shared-model"], json!(128_000));
    assert_eq!(context_windows["custom/two::shared-model"], json!(512_000));
    assert_eq!(
        magi_conversation_runtime::main_model_sources::effective_config(
            &state.settings_store,
            &json!({"sourceId":"custom/one", "model":"shared-model"})
        )
        .unwrap()["contextWindowTokens"],
        json!(128_000)
    );
    assert_eq!(
        magi_conversation_runtime::main_model_sources::effective_config(
            &state.settings_store,
            &json!({"sourceId":"custom/two", "model":"shared-model"})
        )
        .unwrap()["contextWindowTokens"],
        json!(512_000)
    );
}

#[test]
fn first_session_model_uses_model_default_reasoning() {
    let state = test_state();
    let session_id = SessionId::new("session-model-default-effort");
    seed_main_sources(&state, vec![main_source("one", &["first-model"])]);

    let saved = save_orchestrator_session_override_for_session(
        &state,
        &session_id,
        &json!({ "sourceId": "custom/one", "model": "first-model" }),
    )
    .expect("首次模型选择应保存")
    .expect("首次模型选择应生成会话覆盖");

    assert_eq!(saved["model"], json!("first-model"));
    assert_eq!(saved["reasoningEffort"], Value::Null);
    require_orchestrator_session_model(&state, &session_id)
        .expect("模型和默认推理行为应形成可执行配置");
}

#[test]
fn reasoning_defaults_and_model_switches_follow_the_target_capability() {
    let state = test_state();
    state
        .settings_store
        .set_section(
            "orchestrator",
            json!({
                "sources": [{
                    "id": "fixture",
                    "name": "Fixture",
                    "baseUrl": "https://fixture.invalid/v1",
                    "apiKey": "fixture",
                    "apiProtocol": "openai_chat",
                    "models": [
                        { "id": "gpt-5.1" },
                        { "id": "gpt-6.1-sol" },
                        { "id": "gpt-4o" }
                    ]
                }]
            }),
        )
        .unwrap();
    let defaults = orchestrator_session_defaults(&state);
    assert_eq!(defaults["sourceId"], json!("custom/fixture"));
    assert_eq!(defaults["model"], json!("gpt-5.1"));
    assert_eq!(defaults["reasoningEffort"], Value::Null);
    let session = SessionId::new("reasoning-switch");
    let first = save_orchestrator_session_override_for_session(
        &state,
        &session,
        &json!({"sourceId":"custom/fixture", "model":"gpt-5.1"}),
    )
    .unwrap()
    .unwrap();
    assert_eq!(first["reasoningEffort"], Value::Null);
    let switched = save_orchestrator_session_override_for_session(
        &state,
        &session,
        &json!({"sourceId":"custom/fixture", "model":"gpt-6.1-sol"}),
    )
    .unwrap()
    .unwrap();
    assert_eq!(switched["reasoningEffort"], Value::Null);
    let reset = save_orchestrator_session_override_for_session(
        &state,
        &session,
        &json!({"reasoningEffort":null}),
    )
    .unwrap()
    .unwrap();
    assert_eq!(reset["reasoningEffort"], Value::Null);
    let unsupported = save_orchestrator_session_override_for_session(
        &state,
        &session,
        &json!({"sourceId":"custom/fixture", "model":"gpt-4o","reasoningEffort":"ultra"}),
    )
    .unwrap()
    .unwrap();
    assert_eq!(unsupported["reasoningEffort"], Value::Null);
}

#[test]
fn session_model_validation_rejects_missing_source_selection() {
    let state = test_state();
    let session_id = SessionId::new("session-model-missing");
    let error = require_orchestrator_session_model(&state, &session_id)
        .expect_err("未配置任何数据源时必须拒绝");
    assert!(
        matches!(&error, ApiError::InvalidInput(message) if message == "主对话尚未选择数据源，请选择数据源中的模型"),
        "{error:?}"
    );

    // 配置了数据源但会话没有显式选择时，继承目录第一项。
    seed_main_sources(&state, vec![main_source("one", &["m1"])]);
    require_orchestrator_session_model(&state, &session_id).expect("目录第一项应作为默认选择");
    state
        .settings_store
        .set_session_section(
            &session_id,
            "orchestrator",
            json!({"sourceId": "custom/removed", "model": "m1"}),
        )
        .unwrap();
    let error = require_orchestrator_session_model(&state, &session_id)
        .expect_err("会话引用的数据源被移除后不得静默回退");
    assert!(
        matches!(&error, ApiError::InvalidInput(message) if message.contains("已移除")),
        "{error:?}"
    );
}

#[tokio::test]
async fn settings_bootstrap_returns_effective_orchestrator_config_for_session() {
    let state = test_state();
    let _workspace = register_test_workspace(&state, "workspace-bootstrap-session-model");
    let session_id = SessionId::new("session-bootstrap-model-override");
    seed_session(
        &state,
        test_session_record(
            &session_id,
            "workspace-bootstrap-session-model",
            "会话有效主模型",
        ),
    );
    seed_main_sources(
        &state,
        vec![main_source(
            "one",
            &[
                "session-main-model",
                "personal-main-model",
                "model-before",
                "model-after",
            ],
        )],
    );
    state
        .settings_store
        .set_session_section(
            &session_id,
            "orchestrator",
            json!({
                "sourceId": "custom/one",
                "model": "session-main-model",
                "reasoningEffort": "xhigh"
            }),
        )
        .unwrap();

    let bootstrap = settings_bootstrap(
        State(state.clone()),
        Query(HashMap::from([
            ("scope".to_string(), "workspace".to_string()),
            (
                "workspaceId".to_string(),
                "workspace-bootstrap-session-model".to_string(),
            ),
            ("sessionId".to_string(), session_id.as_str().to_string()),
        ])),
    )
    .await
    .expect("settings bootstrap should build")
    .0;

    assert!(
        bootstrap["effectiveOrchestratorConfig"]["model"] != json!("global-main-model"),
        "全局旧主模型不得进入会话有效配置"
    );
    assert_eq!(
        bootstrap["orchestratorSessionConfig"]["model"],
        json!("session-main-model")
    );
    assert_eq!(
        bootstrap["effectiveOrchestratorConfig"]["model"],
        json!("session-main-model"),
        "输入区应读取会话有效主模型"
    );
    assert_eq!(
        bootstrap["effectiveOrchestratorConfig"]["reasoningEffort"],
        json!("xhigh")
    );
}

#[test]
fn fetch_models_config_allows_anthropic_compatible_provider() {
    // /v1/models 在 Anthropic 端点同样合法；请求头由显式协议决定。
    let (config, target) = parse_fetch_models_config(FetchModelsRequest {
        config: serde_json::json!({
            "baseUrl": "https://api.anthropic.com",
            "apiKey": "test-key",
            "model": "claude-sonnet-test",
            "apiProtocol": "anthropic_messages"
        }),
        target: "orch".to_string(),
        workspace_id: None,
    })
    .expect("anthropic-style base url should also be listable");

    assert_eq!(config.provider(), "anthropic");
    assert_eq!(
        config.models_list_url().expect("models url"),
        "https://api.anthropic.com/v1/models"
    );
    assert_eq!(target, "orch");
}

#[test]
fn fetch_models_config_allows_openai_compatible_provider() {
    let (config, target) = parse_fetch_models_config(FetchModelsRequest {
        config: serde_json::json!({
            "baseUrl": "http://127.0.0.1:8320/v1",
            "apiKey": "test-key",
            "apiProtocol": "openai_chat"
        }),
        target: "orch".to_string(),
        workspace_id: None,
    })
    .expect("openai-compatible provider can list models");

    assert_eq!(config.provider(), "openai");
    assert_eq!(
        config.require_base_url().expect("baseUrl"),
        "http://127.0.0.1:8320/v1"
    );
    assert_eq!(config.require_api_key().expect("apiKey"), "test-key");
    assert_eq!(target, "orch");
}

#[test]
fn fetch_models_config_rejects_unknown_model_config_fields() {
    let error = parse_fetch_models_config(FetchModelsRequest {
        config: serde_json::json!({
            "unexpected": "openai",
            "baseUrl": "http://127.0.0.1:8320/v1",
            "apiKey": "test-key",
        }),
        target: "orch".to_string(),
        workspace_id: None,
    })
    .expect_err("fetch models config should reject fields outside the schema");

    match error {
        ApiError::InvalidInput(message) => {
            assert!(message.contains("unexpected"));
        }
        other => panic!("expected invalid input, got {other:?}"),
    }
}

#[tokio::test]
async fn safeguard_audit_pages_newest_first_and_validates_query() {
    let state = test_state();
    for index in 0..3 {
        state.event_bus.publish(
            EventEnvelope::audit(
                EventId::new(format!("event-safeguard-audit-{index}")),
                "security.safety.evaluated",
                json!({
                    "tool_name": "shell",
                    "decision": "hard_block",
                    "access_profile": "FullAccess",
                    "matched_rules": [{
                        "category": "git_history",
                        "pattern": format!("git push --force {index}"),
                        "action": "hard_block",
                    }],
                }),
            )
            .with_context(EventContext {
                session_id: Some(SessionId::new("session-without-record")),
                ..EventContext::default()
            }),
        );
    }
    state.event_bus.publish(EventEnvelope::audit(
        EventId::new("event-safeguard-audit-unrelated"),
        "context.reference.read",
        json!({"path": "README.md"}),
    ));

    let query = |pairs: &[(&str, &str)]| {
        Query(
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect::<HashMap<_, _>>(),
        )
    };
    let first = list_safeguard_audit(State(state.clone()), query(&[("limit", "2")]))
        .await
        .expect("first page")
        .0;
    assert_eq!(first["total"], json!(3));
    let first_entries = first["entries"].as_array().expect("entries");
    assert_eq!(first_entries.len(), 2);
    assert_eq!(
        first_entries[0]["eventId"],
        json!("event-safeguard-audit-2")
    );
    assert_eq!(first_entries[0]["toolName"], json!("shell"));
    assert_eq!(first_entries[0]["decision"], json!("hard_block"));
    assert_eq!(
        first_entries[0]["matchedRules"][0]["pattern"],
        json!("git push --force 2")
    );
    assert_eq!(
        first_entries[0]["sessionId"],
        json!("session-without-record")
    );
    assert_eq!(first_entries[0]["sessionTitle"], Value::Null);
    let cursor = first["nextBefore"].as_u64().expect("next cursor");

    let second = list_safeguard_audit(
        State(state.clone()),
        query(&[("limit", "2"), ("before", &cursor.to_string())]),
    )
    .await
    .expect("second page")
    .0;
    let second_entries = second["entries"].as_array().expect("entries");
    assert_eq!(second_entries.len(), 1);
    assert_eq!(
        second_entries[0]["eventId"],
        json!("event-safeguard-audit-0")
    );
    assert_eq!(second["nextBefore"], Value::Null);

    for invalid in [
        query(&[("limit", "0")]),
        query(&[("limit", "101")]),
        query(&[("limit", "x")]),
        query(&[("before", "x")]),
    ] {
        assert!(
            list_safeguard_audit(State(state.clone()), invalid)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn safeguard_audit_deletion_hides_records_without_touching_later_ones() {
    let state = test_state();
    let publish = |id: &str| {
        state.event_bus.publish(EventEnvelope::audit(
            EventId::new(id),
            "security.safety.evaluated",
            json!({ "tool_name": "shell", "decision": "audit_only", "matched_rules": [] }),
        ));
    };
    for id in ["audit-a", "audit-b", "audit-c"] {
        publish(id);
    }
    let list = || async {
        list_safeguard_audit(State(state.clone()), Query(HashMap::new()))
            .await
            .expect("list")
            .0
    };
    let delete = |body: serde_json::Value| async {
        delete_safeguard_audit(
            State(state.clone()),
            Json(serde_json::from_value(body).expect("request")),
        )
        .await
    };

    let removed = delete(json!({ "eventIds": ["audit-b", "not-there"] }))
        .await
        .expect("delete selected")
        .0;
    assert_eq!(removed["removed"], json!(1));
    assert_eq!(removed["total"], json!(2));
    let ids = list().await["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["eventId"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["audit-c", "audit-a"]);

    let cleared = delete(json!({ "all": true })).await.expect("clear").0;
    assert_eq!(cleared["removed"], json!(2));
    assert_eq!(cleared["total"], json!(0));
    publish("audit-d");
    let after = list().await;
    assert_eq!(after["total"], json!(1));
    assert_eq!(after["entries"][0]["eventId"], json!("audit-d"));

    // 重复清空不产生新的墓碑，也不报错。
    let again = delete(json!({ "eventIds": ["audit-a"] }))
        .await
        .expect("again")
        .0;
    assert_eq!(again["removed"], json!(0));

    for invalid in [
        json!({}),
        json!({ "eventIds": [] }),
        json!({ "all": false }),
        json!({ "all": true, "eventIds": ["audit-d"] }),
    ] {
        assert!(delete(invalid).await.is_err());
    }
}

#[tokio::test]
async fn execution_stats_session_filter_preserves_missing_cache_and_performance() {
    let state = test_state();
    for id in ["stats-a", "stats-b"] {
        state
            .session_store
            .create_session(magi_core::SessionId::new(id), id)
            .unwrap();
    }
    for (index, session) in ["stats-a", "stats-a", "stats-b"].into_iter().enumerate() {
        let id = format!("filtered-{index}");
        let mut payload =
            model_usage_payload(&id, "ws-1", session, &id, UtcMillis::now().0, 1000, 100);
        payload["usage"]
            .as_object_mut()
            .unwrap()
            .remove("cacheReadTokens");
        payload["usage"]
            .as_object_mut()
            .unwrap()
            .remove("cacheWriteTokens");
        if index == 0 {
            payload["usage"]["cacheReadTokens"] = json!(600);
            payload["usage"]["cacheReadIncludedInInput"] = json!(true);
            payload["callIdentity"]["transport"] = json!({
                "requestFingerprint":"a".repeat(64), "httpStatus":200,
                "responseTiming":{"firstContentMs":200,"contentDurationMs":1000,"textOutputTokens":40}
            });
        }
        state.event_bus.publish(EventEnvelope::usage(
            EventId::new(id),
            "model.usage.recorded",
            payload,
        ));
    }
    let query = HashMap::from([("sessionId".into(), "stats-a".into())]);
    let result = execution_stats(State(state.clone()), Query(query.clone()))
        .await
        .unwrap()
        .0;
    assert_eq!(result["totals"]["llmCallCount"], 2);
    assert_eq!(result["totals"]["tokenObservedCallCount"], 2);
    assert_eq!(result["totals"]["cache"]["observedCallCount"], 1);
    assert_eq!(result["totals"]["cache"]["tokenHitRate"], 0.6);
    assert_eq!(result["totals"]["performance"]["ttftSampleCount"], 1);
    assert_eq!(result["totals"]["performance"]["textOutputTokens"], 40);
    assert_eq!(result["items"][0]["performance"]["contentDurationMs"], 1000);
    assert_eq!(
        execution_stats(State(state.clone()), Query(HashMap::new()))
            .await
            .unwrap()
            .0["totals"]["llmCallCount"],
        3
    );
    assert!(
        execution_stats(
            State(state.clone()),
            Query(HashMap::from([("sessionId".into(), "missing".into())]))
        )
        .await
        .is_err()
    );
    state.event_bus.publish(EventEnvelope::usage(
        EventId::new("stats-filter-reset"),
        USAGE_STATS_RESET_EVENT_TYPE,
        json!({}),
    ));
    let reset = execution_stats(State(state), Query(query)).await.unwrap().0;
    assert_eq!(reset["totals"]["llmCallCount"], 0);
    assert_eq!(reset["totals"]["performance"]["ttftSampleCount"], 0);
}

#[tokio::test]
async fn execution_stats_includes_plugin_owners_without_merging_auxiliary_or_session_usage() {
    let state = test_state();
    state
        .session_store
        .create_session(magi_core::SessionId::new("stats-native"), "native")
        .unwrap();
    let mut native = model_usage_payload("native", "ws-1", "stats-native", "native", 101, 10, 2);
    native["executionBinding"]["role"] = json!("auxiliary");
    native["callIdentity"]["source"] = json!("auxiliary");
    for (index, plugin_id) in [None, Some("plugin-a"), Some("plugin-b")]
        .into_iter()
        .enumerate()
    {
        let mut payload = native.clone();
        let event_id = format!("owner-{index}");
        payload["eventId"] = json!(event_id);
        payload["callIdentity"]["callId"] = json!(event_id);
        if let Some(plugin_id) = plugin_id {
            payload.as_object_mut().unwrap().remove("sessionId");
            payload.as_object_mut().unwrap().remove("turnId");
            payload["plugin"] = json!({"pluginId":plugin_id,"digest":"a".repeat(64),
                    "instanceId":"instance-1","operationId":event_id,"displayName":format!("Name {plugin_id}")});
        }
        state.event_bus.publish(EventEnvelope::usage(
            EventId::new(event_id),
            "model.usage.recorded",
            payload,
        ));
    }
    let all = execution_stats(State(state.clone()), Query(HashMap::new()))
        .await
        .unwrap()
        .0;
    assert_eq!(all["totals"]["llmCallCount"], 3);
    assert_eq!(all["items"].as_array().unwrap().len(), 3);
    assert_eq!(all["models"].as_array().unwrap().len(), 1);
    for plugin in ["plugin-a", "plugin-b"] {
        let row = all["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["pluginId"] == plugin)
            .unwrap();
        assert_eq!(row["displayName"], format!("Name {plugin}"));
        assert_eq!(row["llmCallCount"], 1);
    }
    let session_query = HashMap::from([("sessionId".into(), "stats-native".into())]);
    let session = execution_stats(State(state.clone()), Query(session_query))
        .await
        .unwrap()
        .0;
    assert_eq!(session["totals"]["llmCallCount"], 1);
    assert_eq!(session["items"].as_array().unwrap().len(), 1);
    assert!(session["items"][0]["pluginId"].is_null());
    state.event_bus.publish(EventEnvelope::usage(
        EventId::new("plugin-stats-reset"),
        USAGE_STATS_RESET_EVENT_TYPE,
        json!({}),
    ));
    let reset = execution_stats(State(state), Query(HashMap::new()))
        .await
        .unwrap()
        .0;
    assert_eq!(reset["totals"]["llmCallCount"], 0);
}

#[tokio::test]
async fn execution_stats_returns_frontend_stats_contract() {
    let state = test_state();
    let payload = execution_stats(State(state), Query(HashMap::new()))
        .await
        .expect("global stats should build")
        .0;

    assert!(payload["items"].is_array());
    assert!(payload["models"].is_array());
    assert!(payload.get("workspaceId").is_none());
    assert!(payload.get("sessions").is_none());
    assert!(payload.get("scope").is_none());
    assert!(payload.get("sessionId").is_none());
    assert!(payload["totals"].is_object());
    assert!(payload["updatedAt"].is_number());
    assert!(payload["version"].is_number());
    assert!(payload.get("stats").is_none());
}

#[tokio::test]
async fn execution_stats_uses_model_usage_ledger_as_authority() {
    let state = test_state();
    let _workspace_root = register_test_workspace(&state, "workspace-stats");
    let usage_payload = model_usage_payload(
        "usage-model-1",
        "workspace-stats",
        "session-stats",
        "call-1",
        101,
        12,
        5,
    );
    state.event_bus.publish(
        EventEnvelope::usage(
            EventId::new("usage-model-1"),
            "model.usage.recorded",
            usage_payload,
        )
        .with_context(EventContext {
            workspace_id: Some(WorkspaceId::new("workspace-stats")),
            session_id: Some(SessionId::new("session-stats")),
            ..EventContext::default()
        }),
    );

    let payload = execution_stats(State(state.clone()), Query(HashMap::new()))
        .await
        .expect("global stats should build")
        .0;

    assert_eq!(payload["totals"]["netInputTokens"], serde_json::json!(12));
    assert_eq!(payload["totals"]["netOutputTokens"], serde_json::json!(5));
    assert_eq!(payload["totals"]["totalTokens"], serde_json::json!(17));
    for cache in [
        &payload["totals"]["cache"],
        &payload["models"][0]["totals"]["cache"],
        &payload["items"][0]["cache"],
    ] {
        assert_eq!(cache["observedCallCount"], 1);
        assert_eq!(cache["hitCallCount"], 1);
        assert_eq!(cache["readTokens"], 4);
        assert_eq!(cache["writeTokens"], 3);
        assert_eq!(cache["inputTokens"], 19);
        assert_eq!(cache["requestHitRate"], 1.0);
    }
    assert!(payload["totals"].get("cacheReadTokens").is_none());
    assert!(payload["totals"].get("cacheWriteTokens").is_none());
    assert!(
        payload["models"][0]["totals"]
            .get("cacheReadTokens")
            .is_none()
    );
    assert!(
        payload["models"][0]["totals"]
            .get("cacheWriteTokens")
            .is_none()
    );
    assert_eq!(
        payload["items"][0]["templateId"],
        serde_json::json!("orchestrator")
    );
    assert!(payload["items"][0].get("cacheReadTokens").is_none());
    assert!(payload["items"][0].get("cacheWriteTokens").is_none());

    let _ = reset_stats(State(state.clone()), Json(serde_json::json!({})))
        .await
        .expect("reset stats");
    let payload = execution_stats(State(state), Query(HashMap::new()))
        .await
        .expect("global stats should build after reset")
        .0;
    assert_eq!(payload["totals"]["totalTokens"], serde_json::json!(0));
    assert!(payload["totals"]["cache"]["tokenHitRate"].is_null());
    assert!(payload["totals"]["cache"]["requestHitRate"].is_null());
    assert_eq!(payload["items"].as_array().unwrap().len(), 0);
    assert_eq!(payload["models"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn execution_stats_counts_image_calls_without_fabricating_tokens() {
    let state = test_state();
    let mut usage_payload = model_usage_payload(
        "usage-image-1",
        "workspace-image-stats",
        "session-image-stats",
        "image-call-1",
        101,
        0,
        0,
    );
    usage_payload["executionBinding"] = json!({
        "templateId": "imageGeneration",
        "engineId": "imageGeneration",
        "bindingRevision": 0,
        "role": "image_generation"
    });
    usage_payload["modelConfig"]["model"] = json!("gpt-image-test");
    usage_payload["callIdentity"]["source"] = json!("image_generation");
    usage_payload["callIdentity"]["phase"] = json!("execution");
    usage_payload["usage"] = json!({
        "inputTokens": 0,
        "outputTokens": 0,
        "totalTokens": 0
    });
    state.event_bus.publish(
        EventEnvelope::usage(
            EventId::new("usage-image-1"),
            "model.usage.recorded",
            usage_payload,
        )
        .with_context(EventContext {
            workspace_id: Some(WorkspaceId::new("workspace-image-stats")),
            session_id: Some(SessionId::new("session-image-stats")),
            ..EventContext::default()
        }),
    );
    state.event_bus.publish(
        EventEnvelope::usage(
            EventId::new("tool-usage-image-1"),
            "tool.usage.recorded",
            json!({
                "tool_name": "image_generate",
                "tool_call_id": "image-call-1",
                "session_id": "session-image-stats",
                "workspace_id": "workspace-image-stats",
                "status": "Succeeded"
            }),
        )
        .with_context(EventContext {
            workspace_id: Some(WorkspaceId::new("workspace-image-stats")),
            session_id: Some(SessionId::new("session-image-stats")),
            ..EventContext::default()
        }),
    );

    let payload = execution_stats(State(state), Query(HashMap::new()))
        .await
        .expect("image stats should build")
        .0;

    assert_eq!(payload["totals"]["llmCallCount"], json!(1));
    assert_eq!(payload["totals"]["tokenObservedCallCount"], json!(0));
    assert_eq!(payload["totals"]["totalTokens"], json!(0));
    assert_eq!(payload["items"][0]["role"], json!("image_generation"));
    assert_eq!(payload["items"][0]["templateId"], json!("imageGeneration"));
    assert_eq!(payload["items"][0]["llmCallCount"], json!(1));
    assert_eq!(
        payload["models"][0]["resolvedModel"],
        json!("gpt-image-test")
    );
    assert_eq!(payload["models"][0]["totals"]["llmCallCount"], json!(1));
}

#[tokio::test]
async fn execution_stats_keeps_models_separate_after_main_model_switch() {
    let state = test_state();
    let _workspace_root = register_test_workspace(&state, "workspace-model-switch");
    let first = model_usage_payload(
        "usage-model-a",
        "workspace-model-switch",
        "session-model-switch",
        "call-a",
        101,
        12,
        5,
    );
    let mut second = model_usage_payload(
        "usage-model-b",
        "workspace-model-switch",
        "session-model-switch",
        "call-b",
        102,
        20,
        8,
    );
    second["modelConfig"]["model"] = json!("gpt-5.6-luna");

    for (event_id, payload) in [("usage-model-a", first), ("usage-model-b", second)] {
        state.event_bus.publish(
            EventEnvelope::usage(EventId::new(event_id), "model.usage.recorded", payload)
                .with_context(EventContext {
                    workspace_id: Some(WorkspaceId::new("workspace-model-switch")),
                    session_id: Some(SessionId::new("session-model-switch")),
                    ..EventContext::default()
                }),
        );
    }

    let payload = execution_stats(State(state), Query(HashMap::new()))
        .await
        .expect("global stats should keep switched models")
        .0;

    assert_eq!(payload["items"].as_array().map(Vec::len), Some(2));
    assert_eq!(payload["models"].as_array().map(Vec::len), Some(2));
    assert_eq!(payload["totals"]["netInputTokens"], json!(32));
    assert_eq!(payload["totals"]["netOutputTokens"], json!(13));

    let mut models = payload["models"]
        .as_array()
        .expect("models should be returned")
        .iter()
        .filter_map(|item| item["resolvedModel"].as_str())
        .collect::<Vec<_>>();
    models.sort_unstable();
    assert_eq!(models, vec!["gpt-4.1", "gpt-5.6-luna"]);
}

#[tokio::test]
async fn execution_stats_keeps_same_named_models_separate_by_main_data_source() {
    let state = test_state();
    let _workspace_root = register_test_workspace(&state, "workspace-model-sources");
    let mut first = model_usage_payload(
        "usage-source-one",
        "workspace-model-sources",
        "session-model-sources",
        "call-source-one",
        101,
        20,
        6,
    );
    first["modelConfig"]["provider"] = json!("custom/source-one");
    first["modelConfig"]["model"] = json!("shared-model");
    let mut second = model_usage_payload(
        "usage-source-two",
        "workspace-model-sources",
        "session-model-sources",
        "call-source-two",
        102,
        30,
        8,
    );
    second["modelConfig"]["provider"] = json!("custom/source-two");
    second["modelConfig"]["model"] = json!("shared-model");

    for (event_id, payload) in [("usage-source-one", first), ("usage-source-two", second)] {
        state.event_bus.publish(
            EventEnvelope::usage(EventId::new(event_id), "model.usage.recorded", payload)
                .with_context(EventContext {
                    workspace_id: Some(WorkspaceId::new("workspace-model-sources")),
                    session_id: Some(SessionId::new("session-model-sources")),
                    ..EventContext::default()
                }),
        );
    }

    let payload = execution_stats(State(state), Query(HashMap::new()))
        .await
        .expect("global stats should retain both main data sources")
        .0;
    let models = payload["models"]
        .as_array()
        .expect("models should be returned");
    assert_eq!(models.len(), 2);
    assert!(
        models
            .iter()
            .all(|model| model["resolvedModel"] == "shared-model")
    );
    assert_eq!(
        models
            .iter()
            .filter_map(|model| model["provider"].as_str())
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from(["custom/source-one", "custom/source-two"])
    );
    assert_ne!(models[0]["modelIdentityKey"], models[1]["modelIdentityKey"]);
    assert_eq!(payload["totals"]["cache"]["observedCallCount"], json!(2));
    assert_eq!(payload["totals"]["cache"]["readTokens"], json!(8));
}

#[tokio::test]
async fn execution_stats_exposes_model_and_role_dimensions() {
    let state = test_state();
    let _workspace_root = register_test_workspace(&state, "workspace-stats-dimensions");
    let first = model_usage_payload(
        "usage-session-a",
        "workspace-stats-dimensions",
        "session-a",
        "call-a",
        101,
        12,
        5,
    );
    let mut second = model_usage_payload(
        "usage-session-b",
        "workspace-stats-dimensions",
        "session-b",
        "call-b",
        102,
        20,
        8,
    );
    second["modelConfig"]["model"] = json!("gpt-5.6-luna");

    for (event_id, session_id, payload) in [
        ("usage-session-a", "session-a", first),
        ("usage-session-b", "session-b", second),
    ] {
        state.event_bus.publish(
            EventEnvelope::usage(EventId::new(event_id), "model.usage.recorded", payload)
                .with_context(EventContext {
                    workspace_id: Some(WorkspaceId::new("workspace-stats-dimensions")),
                    session_id: Some(SessionId::new(session_id)),
                    ..EventContext::default()
                }),
        );
    }

    let payload = execution_stats(State(state), Query(HashMap::new()))
        .await
        .expect("global stats should expose model and role dimensions")
        .0;

    assert!(payload.get("sessions").is_none());
    assert_eq!(payload["models"].as_array().map(Vec::len), Some(2));
    assert_eq!(payload["items"].as_array().map(Vec::len), Some(2));
    assert_eq!(payload["totals"]["llmCallCount"], json!(2));
}

#[tokio::test]
async fn execution_stats_does_not_require_workspace_binding() {
    let payload = execution_stats(State(test_state()), Query(HashMap::new()))
        .await
        .expect("global stats should not require workspace binding")
        .0;

    assert_eq!(payload["totals"]["totalTokens"], json!(0));
    assert_eq!(payload["items"].as_array().map(Vec::len), Some(0));
    assert_eq!(payload["models"].as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn execution_stats_includes_personal_session_usage_without_workspace_id() {
    let state = test_state();
    let mut usage_payload = model_usage_payload(
        "usage-personal-session",
        "unused-workspace",
        "session-personal-stats",
        "call-personal-stats",
        101,
        12,
        5,
    );
    usage_payload
        .as_object_mut()
        .expect("usage payload should be an object")
        .remove("workspaceId");
    state.event_bus.publish(
        EventEnvelope::usage(
            EventId::new("usage-personal-session"),
            "model.usage.recorded",
            usage_payload,
        )
        .with_context(EventContext {
            workspace_id: None,
            session_id: Some(SessionId::new("session-personal-stats")),
            ..EventContext::default()
        }),
    );

    let payload = execution_stats(State(state), Query(HashMap::new()))
        .await
        .expect("personal session usage should be included in global stats")
        .0;

    assert_eq!(payload["totals"]["llmCallCount"], json!(1));
    assert_eq!(payload["totals"]["netInputTokens"], json!(12));
    assert_eq!(payload["totals"]["netOutputTokens"], json!(5));
    assert_eq!(payload["totals"]["totalTokens"], json!(17));
    assert_eq!(payload["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["models"].as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn execution_stats_aggregates_usage_across_workspaces() {
    let state = test_state();
    for (event_id, workspace_id, session_id, input_tokens, output_tokens) in [
        ("usage-workspace-a", "workspace-a", "session-a", 12, 5),
        ("usage-workspace-b", "workspace-b", "session-b", 30, 7),
    ] {
        state.event_bus.publish(
            EventEnvelope::usage(
                EventId::new(event_id),
                "model.usage.recorded",
                model_usage_payload(
                    event_id,
                    workspace_id,
                    session_id,
                    event_id,
                    101,
                    input_tokens,
                    output_tokens,
                ),
            )
            .with_context(EventContext {
                workspace_id: Some(WorkspaceId::new(workspace_id)),
                session_id: Some(SessionId::new(session_id)),
                ..EventContext::default()
            }),
        );
    }

    let payload = execution_stats(State(state), Query(HashMap::new()))
        .await
        .expect("global stats should aggregate all workspaces")
        .0;

    assert_eq!(payload["totals"]["llmCallCount"], json!(2));
    assert_eq!(payload["totals"]["netInputTokens"], json!(42));
    assert_eq!(payload["totals"]["netOutputTokens"], json!(12));
    assert_eq!(payload["totals"]["totalTokens"], json!(54));
}

#[tokio::test]
async fn reset_stats_counts_only_usage_after_reset_marker() {
    let state = test_state();
    for (event_id, context, input_tokens, output_tokens, call_id, timestamp) in [
        (
            "usage-scoped",
            EventContext {
                workspace_id: Some(WorkspaceId::new("workspace-stats")),
                session_id: Some(SessionId::new("session-stats")),
                ..EventContext::default()
            },
            12,
            5,
            "call-scoped",
            101,
        ),
        (
            "usage-unscoped",
            EventContext::default(),
            30,
            7,
            "call-unscoped",
            102,
        ),
    ] {
        state.event_bus.publish(
            EventEnvelope::usage(
                EventId::new(event_id),
                "model.usage.recorded",
                model_usage_payload(
                    event_id,
                    "workspace-stats",
                    "session-stats",
                    call_id,
                    timestamp,
                    input_tokens,
                    output_tokens,
                ),
            )
            .with_context(context),
        );
    }
    state.event_bus.publish(EventEnvelope::usage(
        EventId::new("usage-other"),
        "tool.usage.recorded",
        json!({ "tool": "file_read" }),
    ));

    let before_reset = execution_stats(State(state.clone()), Query(HashMap::new()))
        .await
        .expect("global stats should build before reset")
        .0;
    assert_eq!(before_reset["totals"]["totalTokens"], serde_json::json!(54));

    let _ = reset_stats(State(state.clone()), Json(serde_json::json!({})))
        .await
        .expect("reset stats should succeed");

    let after_reset = execution_stats(State(state.clone()), Query(HashMap::new()))
        .await
        .expect("global stats should build after reset")
        .0;
    assert_eq!(after_reset["totals"]["totalTokens"], serde_json::json!(0));

    // 账本只追加：重置以标记事件表达，历史条目保留，标记之后的新用量重新计入。
    let ledger_snapshot = state.event_bus.audit_usage_ledger_snapshot();
    assert_eq!(
        ledger_snapshot
            .usage_entries
            .last()
            .map(|entry| entry.event_type.as_str()),
        Some(USAGE_STATS_RESET_EVENT_TYPE)
    );
    assert!(
        ledger_snapshot
            .usage_entries
            .iter()
            .any(|entry| entry.event_type == "model.usage.recorded")
    );
    state.event_bus.publish(EventEnvelope::usage(
        EventId::new("usage-after-reset"),
        "model.usage.recorded",
        model_usage_payload(
            "usage-after-reset",
            "workspace-stats",
            "session-stats",
            "call-after-reset",
            200,
            4,
            1,
        ),
    ));
    let after_new_usage = execution_stats(State(state.clone()), Query(HashMap::new()))
        .await
        .expect("global stats should build after new usage")
        .0;
    assert_eq!(
        after_new_usage["totals"]["totalTokens"],
        serde_json::json!(5)
    );
}

#[tokio::test]
async fn global_rules_and_safeguard_are_exposed_in_bootstrap() {
    let state = test_state();
    let session_a = SessionId::new("session-a");
    let session_b = SessionId::new("session-b");
    state
        .session_store
        .create_session(session_a.clone(), "A 会话")
        .expect("session A should create");
    state
        .session_store
        .create_session(session_b.clone(), "B 会话")
        .expect("session B should create");

    let user_rules_payload = serde_json::json!({
        "userRules": "【全局生效】"
    });
    let safeguard_payload = serde_json::json!({
        "revision": 0,
        "rules": [
            {
                "pattern": "custom-danger-global",
                "enabled": true,
                "category": "custom",
                "action": "hard_block"
            },
            {
                "pattern": "  custom-audit-global  ",
                "enabled": true,
                "category": "custom",
                "action": "audit_only"
            },
            {
                "pattern": "ignored-empty-rule",
                "enabled": true,
                "category": "unknown-category",
                "action": "unknown-action"
            },
            {
                "pattern": "   ",
                "enabled": true,
                "category": "custom",
                "action": "hard_block"
            }
        ]
    });

    let saved_rules = save_user_rules(State(state.clone()), Json(user_rules_payload))
        .await
        .expect("save user rules should succeed");
    assert_eq!(saved_rules.0["saved"], serde_json::json!(true));

    let saved_safeguard = save_safeguard_config(State(state.clone()), Json(safeguard_payload))
        .await
        .expect("save safeguard config should succeed");
    assert_eq!(saved_safeguard.0["saved"], serde_json::json!(true));
    assert_eq!(saved_safeguard.0["revision"], serde_json::json!(1));
    let stale_save = save_safeguard_config(
        State(state.clone()),
        Json(serde_json::json!({ "revision": 0, "rules": [] })),
    )
    .await;
    assert!(matches!(stale_save, Err(ApiError::Conflict(_))));
    let saved_section = state.settings_store.get_section("safeguardConfig");
    let saved_rules = saved_section["rules"]
        .as_array()
        .expect("saved safeguard rules should be array");
    assert_eq!(saved_rules.len(), 3);
    assert!(
        saved_rules.iter().any(
            |rule| rule["pattern"] == serde_json::json!("custom-audit-global")
                && rule["action"] == serde_json::json!("audit_only")
        ),
        "save path should trim pattern and preserve audit action"
    );
    assert!(
        saved_rules.iter().any(
            |rule| rule["pattern"] == serde_json::json!("ignored-empty-rule")
                && rule["category"] == serde_json::json!("custom")
                && rule["action"] == serde_json::json!("require_approval_in_restricted")
        ),
        "save path should normalize unknown category/action exactly as SafetyGate does"
    );

    let bootstrap_a = settings_bootstrap(
        State(state.clone()),
        Query(HashMap::from([
            ("scope".to_string(), "personal".to_string()),
            ("sessionId".to_string(), session_a.as_str().to_string()),
        ])),
    )
    .await
    .expect("settings bootstrap A should build")
    .0;
    let bootstrap_b = settings_bootstrap(
        State(state.clone()),
        Query(HashMap::from([
            ("scope".to_string(), "personal".to_string()),
            ("sessionId".to_string(), session_b.as_str().to_string()),
        ])),
    )
    .await
    .expect("settings bootstrap B should build")
    .0;

    assert_eq!(
        bootstrap_a["userRulesConfig"]["userRules"],
        serde_json::json!("【全局生效】")
    );
    assert_eq!(
        bootstrap_a["safeguardConfig"]["rules"][0]["pattern"],
        serde_json::json!("git push --force")
    );
    assert_eq!(
        bootstrap_a["safeguardConfig"]["rules"][0]["action"],
        serde_json::json!("require_approval_in_restricted")
    );
    assert!(
        bootstrap_a["safeguardConfig"]["rules"]
            .as_array()
            .expect("rules should be array")
            .iter()
            .any(
                |rule| rule["pattern"] == serde_json::json!("custom-danger-global")
                    && rule["action"] == serde_json::json!("hard_block")
            )
    );
    assert!(
        bootstrap_a["safeguardConfig"]["rules"]
            .as_array()
            .expect("rules should be array")
            .iter()
            .any(
                |rule| rule["pattern"] == serde_json::json!("custom-audit-global")
                    && rule["action"] == serde_json::json!("audit_only")
            )
    );
    assert_eq!(
        bootstrap_b["userRulesConfig"]["userRules"],
        serde_json::json!("【全局生效】")
    );
    assert_eq!(
        bootstrap_b["safeguardConfig"]["rules"][0]["pattern"],
        serde_json::json!("git push --force")
    );
    assert!(
        bootstrap_b["safeguardConfig"]["rules"]
            .as_array()
            .expect("rules should be array")
            .iter()
            .any(
                |rule| rule["pattern"] == serde_json::json!("custom-danger-global")
                    && rule["action"] == serde_json::json!("hard_block")
            )
    );
}
