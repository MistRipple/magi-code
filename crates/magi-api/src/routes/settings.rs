use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_bridge_client::{
    BridgeClientError, BridgeErrorLayer, HttpModelBridgeProtocol,
    ImageGenerationRequest as BridgeImageGenerationRequest, ModelBridgeClient,
    ModelInvocationRequest,
};
use magi_conversation_runtime::model_context_window::{
    MODEL_CONTEXT_WINDOWS_SECTION, model_context_windows_with_update,
    set_main_source_model_context_window, set_model_context_window,
};
use magi_core::{AccessProfile, EventId, UtcMillis};
use magi_event_bus::{
    AuditUsageLedgerEntry, EventContext, EventEnvelope, USAGE_STATS_RESET_EVENT_TYPE,
};
#[cfg(test)]
use magi_settings_store::ORCHESTRATOR_SESSION_DEFAULTS_SECTION;
use magi_usage_authority::{UsageAuthority, UsageCallRecordInput, UsageModelSnapshot, UsageTotals};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::str::FromStr;

use crate::scope_binding as session_scope;
use crate::{
    errors::{ApiError, settings_persistence_error},
    model_config::{
        DEFAULT_VISION_CONTEXT_WINDOW, NormalizedModelConfig, VISION_MODEL_SECTION,
        builtin_text_model_rule_catalog, load_agent_overrides, load_registry_engines,
        orchestrator_session_defaults, reject_unknown_model_config_fields, resolve_registry_agents,
        resolved_orchestrator_session_config, role_templates_for_registry,
        save_orchestrator_session_override_for_session, template_ids_for_registry,
        template_order_map_for_registry, validate_vision_model_settings,
    },
    scope_binding::without_scope_binding_fields,
    state::ApiState,
};

/// 设置 section 保存请求体就是设置对象本身（不包裹 config/data）。
fn scoped_settings_section_request(
    request: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    Ok(without_scope_binding_fields(request.clone()))
}

fn model_settings_section_request(
    request: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    let config = scoped_settings_section_request(request)?;
    reject_unknown_model_config_fields(&config).map_err(ApiError::InvalidInput)?;
    NormalizedModelConfig::from_settings_value(&config).map_err(ApiError::InvalidInput)?;
    Ok(config)
}

fn orchestrator_connection_section_request(
    request: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    let mut config = scoped_settings_section_request(request)?;
    config
        .as_object_mut()
        .ok_or_else(|| ApiError::InvalidInput("主对话数据源配置必须是对象".into()))?
        .remove("expectedRevision");
    let config = magi_conversation_runtime::main_model_sources::parse(&config)
        .map_err(ApiError::InvalidInput)?;
    serde_json::to_value(config).map_err(|_| ApiError::InvalidInput("数据源配置无效".into()))
}

fn parse_optional_query_string(query: &HashMap<String, String>, key: &str) -> Option<String> {
    query
        .get(key)
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// `GET /settings/bootstrap` 的查询参数 schema；其他查询字段一律拒绝。
const SETTINGS_BOOTSTRAP_QUERY_FIELDS: &[&str] = &[
    "scope",
    "sessionId",
    "workspaceId",
    "workspacePath",
    "accessProfile",
    "bootstrapScope",
];

fn reject_unknown_settings_bootstrap_query_fields(
    query: &HashMap<String, String>,
) -> Result<(), ApiError> {
    if let Some(key) = query
        .keys()
        .find(|key| !SETTINGS_BOOTSTRAP_QUERY_FIELDS.contains(&key.as_str()))
    {
        return Err(ApiError::InvalidInput(format!("设置查询不支持字段 {key}")));
    }
    Ok(())
}

/// `POST /settings/orchestrator/session/save` 的请求体 schema。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrchestratorSessionConfigRequest {
    scope: crate::dto::SessionScopeKindDto,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    config: Value,
}

fn parse_access_profile_query(query: &HashMap<String, String>) -> AccessProfile {
    query
        .get("accessProfile")
        .map(String::as_str)
        .and_then(|value| AccessProfile::from_str(value).ok())
        .unwrap_or_default()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FetchModelsRequest {
    config: Value,
    target: String,
    #[serde(default)]
    workspace_id: Option<String>,
}

fn parse_fetch_models_config(
    request: FetchModelsRequest,
) -> Result<(NormalizedModelConfig, String), ApiError> {
    reject_unknown_model_config_fields(&request.config).map_err(ApiError::InvalidInput)?;
    let config = NormalizedModelConfig::from_settings_value(&request.config)
        .map_err(ApiError::InvalidInput)?;
    config.require_base_url().map_err(ApiError::InvalidInput)?;
    Ok((config, request.target))
}

fn model_entry_id(entry: &Value) -> Option<&str> {
    entry
        .as_str()
        .or_else(|| entry.get("id").and_then(Value::as_str))
        .or_else(|| entry.get("name").and_then(Value::as_str))
        .or_else(|| entry.get("slug").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn model_catalog_entries(payload: &Value) -> &[Value] {
    payload
        .get("data")
        .and_then(Value::as_array)
        .filter(|entries| !entries.is_empty())
        .or_else(|| payload.get("models").and_then(Value::as_array))
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn parse_model_ids(payload: &Value) -> Vec<String> {
    let mut models = model_catalog_entries(payload)
        .iter()
        .filter_map(model_entry_id)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    models
}

struct ModelCatalog {
    models: Vec<String>,
    capabilities: Map<String, Value>,
    /// 服务商给请求 id 提供的可读展示名。请求仍必须使用 `models` 中的原始 id，
    /// 展示层不能把别名反向写回执行配置。
    display_names: HashMap<String, String>,
}

impl ModelCatalog {
    fn from_payload(payload: &Value, anthropic: bool) -> Self {
        let capabilities = model_catalog_entries(payload)
            .iter()
            .filter_map(|entry| {
                Some((
                    model_entry_id(entry)?.to_string(),
                    json!(magi_bridge_client::reasoning::provider_capability(
                        entry, anthropic
                    )?),
                ))
            })
            .collect();
        let display_names = model_catalog_entries(payload)
            .iter()
            .filter_map(|entry| {
                let id = model_entry_id(entry)?.to_string();
                let display_name = entry
                    .get("display_name")
                    .or_else(|| entry.get("displayName"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)?;
                Some((id, display_name))
            })
            .collect();
        Self {
            models: parse_model_ids(payload),
            capabilities,
            display_names,
        }
    }

    /// 同一连接的版本化目录。先按精确 id，再按服务商声明的相同展示名关联别名；
    /// 展示名有冲突且能力不同则不关联，不解码别名、不更换实际请求的 model id。
    fn supplement(&mut self, primary: &Value, metadata: &Value, anthropic: bool) {
        let supplement = Self::from_payload(metadata, anthropic);
        for entry in model_catalog_entries(primary) {
            let Some(id) = model_entry_id(entry) else {
                continue;
            };
            if self.capabilities.contains_key(id) {
                continue;
            }
            let explicit = supplement.capabilities.get(id).cloned().or_else(|| {
                let name = entry.get("display_name").and_then(Value::as_str)?.trim();
                if name.is_empty() {
                    return None;
                }
                let mut matches = model_catalog_entries(metadata)
                    .iter()
                    .filter(|candidate| {
                        candidate
                            .get("display_name")
                            .and_then(Value::as_str)
                            .is_some_and(|value| value.trim() == name)
                    })
                    .filter_map(|candidate| {
                        supplement.capabilities.get(model_entry_id(candidate)?)
                    });
                let first = matches.next()?.clone();
                matches.all(|value| value == &first).then_some(first)
            });
            if let Some(capability) = explicit {
                self.capabilities.insert(id.to_string(), capability);
            }
        }
        for (id, capability) in supplement.capabilities {
            self.capabilities.entry(id).or_insert(capability);
        }
        for (id, display_name) in supplement.display_names {
            self.display_names.entry(id).or_insert(display_name);
        }
    }
}

fn parse_connection_probe_config(config: Value) -> Result<NormalizedModelConfig, ApiError> {
    reject_unknown_model_config_fields(&config).map_err(ApiError::InvalidInput)?;
    let normalized =
        NormalizedModelConfig::from_settings_value(&config).map_err(ApiError::InvalidInput)?;
    normalized
        .require_base_url()
        .map_err(ApiError::InvalidInput)?;
    normalized
        .require_api_key()
        .map_err(ApiError::InvalidInput)?;
    normalized.require_model().map_err(ApiError::InvalidInput)?;
    Ok(normalized)
}

async fn execute_connection_probe(config: &NormalizedModelConfig) -> Result<(), ApiError> {
    let client = config
        .to_http_model_client()
        .ok_or_else(|| ApiError::InvalidInput("模型配置缺少 model".to_string()))?;
    let request = ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "probe".to_string(),
        prompt: "ping".to_string(),
        messages: None,
        tools: None,
        tool_choice: None,
    };
    tokio::task::spawn_blocking(move || client.invoke_streaming(request, &|_| {}))
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, "model streaming connection probe thread failed");
            ApiError::InvalidInput("模型连接测试失败".to_string())
        })?
        .map(|_| ())
        .map_err(model_bridge_probe_error)
}

async fn fetch_model_catalog_for_config(
    config: &NormalizedModelConfig,
) -> Result<ModelCatalog, ApiError> {
    let url = config.models_list_url().map_err(ApiError::InvalidInput)?;
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let Some(protocol) = config.api_protocol() else {
        return Err(ApiError::InvalidInput(
            "插件引擎没有 HTTP 模型列表接口".to_string(),
        ));
    };
    if let Some(api_key) = config.api_key() {
        match protocol {
            HttpModelBridgeProtocol::ChatCompletions | HttpModelBridgeProtocol::Responses => {
                let auth_value = HeaderValue::from_str(&format!("Bearer {}", api_key))
                    .map_err(|_| ApiError::InvalidInput("apiKey 包含非法字符".to_string()))?;
                headers.insert(AUTHORIZATION, auth_value);
            }
            HttpModelBridgeProtocol::AnthropicMessages => {
                let key_value = HeaderValue::from_str(api_key)
                    .map_err(|_| ApiError::InvalidInput("apiKey 包含非法字符".to_string()))?;
                headers.insert(HeaderName::from_static("x-api-key"), key_value);
                headers.insert(
                    HeaderName::from_static("anthropic-version"),
                    HeaderValue::from_static("2023-06-01"),
                );
            }
        }
    }

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .headers(headers.clone())
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, "model list request transport failed");
            ApiError::InvalidInput(model_transport_error_message(&error).to_string())
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(ApiError::InvalidInput(
            model_http_status_error_message(status.as_u16()).to_string(),
        ));
    }
    let payload: Value = response.json().await.map_err(|error| {
        tracing::warn!(error = %error, "model list response parse failed");
        ApiError::InvalidInput("模型列表响应格式异常".to_string())
    })?;
    let anthropic = protocol == HttpModelBridgeProtocol::AnthropicMessages;
    let mut catalog = ModelCatalog::from_payload(&payload, anthropic);
    if catalog
        .models
        .iter()
        .any(|model| !catalog.capabilities.contains_key(model))
    {
        // Codex-compatible gateways publish richer /models metadata when client_version is
        // supplied. Identify our actual version; do not impersonate Codex or call inference.
        let mut versioned_url = reqwest::Url::parse(&url)
            .map_err(|_| ApiError::InvalidInput("模型列表地址无效".into()))?;
        versioned_url
            .query_pairs_mut()
            .append_pair("client_version", env!("CARGO_PKG_VERSION"));
        let metadata = client
            .get(versioned_url)
            .headers(headers)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;
        if let Ok(response) = metadata
            && response.status().is_success()
            && let Ok(metadata) = response.json::<Value>().await
        {
            catalog.supplement(&payload, &metadata, anthropic);
        }
    }
    Ok(catalog)
}

fn persist_model_reasoning_catalog(
    state: &ApiState,
    config: &NormalizedModelConfig,
    catalog: &ModelCatalog,
) -> Result<(), ApiError> {
    let anthropic = config.api_protocol() == Some(HttpModelBridgeProtocol::AnthropicMessages);
    let mut capabilities = catalog.capabilities.clone();
    for model in &catalog.models {
        capabilities.entry(model.clone()).or_insert_with(|| {
            json!(magi_bridge_client::reasoning::builtin_capability(
                model, anthropic
            ))
        });
    }
    state
        .settings_store
        .update_section("modelReasoningCatalog", |cache| {
            if !cache.is_object() {
                *cache = json!({});
            }
            cache
                .as_object_mut()
                .unwrap()
                .insert(config.reasoning_catalog_key(), Value::Object(capabilities));
        })
        .map_err(settings_persistence_error)
}

async fn fetch_model_ids_for_config(
    config: &NormalizedModelConfig,
) -> Result<Vec<String>, ApiError> {
    Ok(fetch_model_catalog_for_config(config).await?.models)
}

async fn execute_model_catalog_probe(config: &NormalizedModelConfig) -> Result<(), ApiError> {
    let models = fetch_model_ids_for_config(config).await?;
    if models.is_empty() {
        return Err(ApiError::InvalidInput(
            "该 API 不支持模型列表查询，请在会话中手动填写模型名".to_string(),
        ));
    }
    Ok(())
}

fn model_http_status_error_message(status: u16) -> &'static str {
    match status {
        401 | 403 => "模型鉴权失败",
        404 => "模型不存在或端点不可用",
        408 | 504 => "模型连接超时",
        429 => "模型服务限流",
        500..=599 => "模型服务暂不可用",
        _ => "模型服务返回失败状态",
    }
}

fn model_transport_error_message(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        return "模型连接超时";
    }
    if error.is_connect() {
        return "模型服务连接失败";
    }
    "模型连接测试失败"
}

fn model_bridge_probe_error(error: BridgeClientError) -> ApiError {
    let raw = error.to_string();
    tracing::warn!(error = %raw, "model streaming connection probe failed");
    if let Some(status) = error.http_status() {
        return ApiError::InvalidInput(model_http_status_error_message(status).to_string());
    }
    match error.layer() {
        Some(BridgeErrorLayer::Transport) => ApiError::InvalidInput("模型服务连接失败".to_string()),
        Some(BridgeErrorLayer::Protocol) => ApiError::InvalidInput("模型请求配置无效".to_string()),
        Some(BridgeErrorLayer::RemoteBusiness) | None => {
            ApiError::InvalidInput("模型服务暂不可用".to_string())
        }
    }
}

async fn probe_connection_response(
    state: &ApiState,
    request: Value,
) -> Result<Json<Value>, ApiError> {
    parse_connection_probe_config(request.clone())?;
    let config = NormalizedModelConfig::from_settings_with_catalog(&state.settings_store, &request)
        .map_err(ApiError::InvalidInput)?;
    execute_connection_probe(&config).await?;

    Ok(Json(json!({
        "success": true,
        "message": "连接测试成功"
    })))
}

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/settings/bootstrap", get(settings_bootstrap))
        .route("/status", get(runtime_status))
        .route("/settings/update", post(update_setting))
        .route("/settings/worker/save", post(save_worker_config))
        .route("/settings/worker/remove", post(remove_worker_config))
        .route("/settings/worker/test", post(test_worker_connection))
        .route(
            "/settings/orchestrator/save",
            post(save_orchestrator_config),
        )
        .route(
            "/settings/orchestrator/session/save",
            post(save_orchestrator_session_config),
        )
        .route(
            "/settings/model-context-window/save",
            post(save_model_context_window),
        )
        .route(
            "/settings/orchestrator/test",
            post(test_orchestrator_connection),
        )
        .route("/settings/auxiliary/save", post(save_auxiliary_config))
        .route("/settings/auxiliary/test", post(test_auxiliary_connection))
        .route("/settings/vision/save", post(save_vision_config))
        .route("/settings/vision/test", post(test_vision_connection))
        .route(
            "/settings/image-generation/save",
            post(save_image_generation_config),
        )
        .route(
            "/settings/image-generation/test",
            post(test_image_generation_connection),
        )
        .route("/settings/user-rules/save", post(save_user_rules))
        .route("/settings/safeguard/save", post(save_safeguard_config))
        .route("/settings/safeguard/audit", get(list_safeguard_audit))
        .route(
            "/settings/safeguard/audit/delete",
            post(delete_safeguard_audit),
        )
        .route(
            "/settings/registry/role-templates",
            get(list_role_templates),
        )
        .route("/settings/registry/roles/upsert", post(upsert_role))
        .route("/settings/registry/roles/delete", post(delete_role))
        .route("/settings/registry/roles/import", post(import_role))
        .route("/settings/registry/roles/export", get(export_role))
        .route("/settings/registry/engines", get(list_engines))
        .route("/settings/registry/engines/upsert", post(upsert_engine))
        .route("/settings/registry/engines/remove", post(remove_engine))
        .route("/settings/registry/agents", get(list_agents))
        .route("/settings/registry/agents/upsert", post(upsert_agent))
        .route("/settings/registry/agents/remove", post(remove_agent))
        .route("/settings/models/fetch", post(fetch_models))
        .route("/settings/models/catalog", get(main_model_catalog))
        .route(
            "/settings/models/reasoning",
            post(model_reasoning_capability),
        )
        .route("/settings/stats", get(execution_stats))
        .route("/settings/stats/reset", post(reset_stats))
}

async fn settings_bootstrap(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    reject_unknown_settings_bootstrap_query_fields(&query)?;
    let hydrate_mcp_servers = query
        .get("bootstrapScope")
        .map(|value| value.trim())
        .is_none_or(|scope| scope != "core");
    let session_id = parse_optional_query_string(&query, "sessionId");
    let workspace_id = parse_optional_query_string(&query, "workspaceId");
    let workspace_path = parse_optional_query_string(&query, "workspacePath");
    let requested_scope = query
        .get("scope")
        .ok_or_else(|| ApiError::InvalidInput("scope 不能为空".to_string()))?;
    let requested_scope =
        serde_json::from_value::<crate::dto::SessionScopeKindDto>(json!(requested_scope))
            .map_err(ApiError::invalid_request_body)?;
    let scope = session_scope::resolve_optional_explicit_session_scope(
        &state,
        requested_scope,
        session_id.as_deref(),
        workspace_id.as_deref(),
        workspace_path.as_deref(),
    )?;
    let mut tool_context = scope.tool_context();
    tool_context.access_profile = parse_access_profile_query(&query);
    let mut snapshot = state.settings_snapshot_json_with_mcp_hydration_and_tool_context(
        hydrate_mcp_servers,
        &tool_context,
    );
    if let Some(object) = snapshot.as_object_mut() {
        object.insert(
            "visionBuiltinTextModelRules".to_string(),
            json!(builtin_text_model_rule_catalog()),
        );
        object.insert("scope".to_string(), json!(scope.scope_kind()));
        let selection = scope
            .session_id()
            .map(|session_id| resolved_orchestrator_session_config(&state, session_id))
            .unwrap_or_else(|| orchestrator_session_defaults(&state));
        let effective = magi_conversation_runtime::main_model_sources::effective_config(
            &state.settings_store,
            &selection,
        );
        if let Err(error) = &effective {
            object.insert("orchestratorModelError".into(), json!(error));
        }
        object.insert("orchestratorSessionConfig".into(), selection.clone());
        object.insert(
            "effectiveOrchestratorConfig".into(),
            effective.unwrap_or(selection),
        );
        if let Some(effective) = object.get("effectiveOrchestratorConfig")
            && let Ok(config) =
                NormalizedModelConfig::from_settings_with_catalog(&state.settings_store, effective)
            && let Ok(model) = config.require_model()
        {
            let capability = json!(config.reasoning_capability(model));
            object.insert("orchestratorReasoningCapability".into(), capability);
        }
        object.insert(
            "orchestratorSessionDefaults".to_string(),
            orchestrator_session_defaults(&state),
        );
        object.insert(
            "workspaceId".to_string(),
            json!(scope.workspace_id_string()),
        );
        object.insert(
            "workspacePath".to_string(),
            json!(scope.workspace_path_string()),
        );
        object.insert(
            "sessionId".to_string(),
            json!(scope.session_id().map(ToString::to_string)),
        );
    }
    Ok(Json(snapshot))
}

async fn runtime_status(State(state): State<ApiState>) -> Json<serde_json::Value> {
    Json(state.runtime_status_json())
}

/// `/settings/update` 只负责运行时偏好（界面语言、对话显示模式）。其他设置都有各自的
/// 专用接口；这里不接受任意键，避免写出与专用接口或运行时读取位置不一致的数据。
#[derive(Debug, Deserialize)]
#[serde(tag = "key", content = "value", deny_unknown_fields)]
enum RuntimeSettingUpdate {
    #[serde(rename = "locale")]
    Locale(String),
    #[serde(rename = "conversationDisplayMode")]
    ConversationDisplayMode(ConversationDisplayMode),
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConversationDisplayMode {
    Original,
    Summary,
}

async fn update_setting(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let update = serde_json::from_value::<RuntimeSettingUpdate>(request).map_err(|error| {
        ApiError::InvalidInput(format!(
            "只能更新 locale 或 conversationDisplayMode（original / summary）：{error}"
        ))
    })?;
    let (key, value) = match update {
        RuntimeSettingUpdate::Locale(locale) => {
            if !crate::locales::is_supported_locale(&locale) {
                return Err(ApiError::InvalidInput(format!(
                    "不支持的界面语言 {locale}，可选：{}",
                    crate::locales::SUPPORTED_LOCALES.join(" / ")
                )));
            }
            ("locale", locale)
        }
        RuntimeSettingUpdate::ConversationDisplayMode(mode) => (
            "conversationDisplayMode",
            match mode {
                ConversationDisplayMode::Original => "original",
                ConversationDisplayMode::Summary => "summary",
            }
            .to_string(),
        ),
    };
    state
        .settings_store
        .set(key, serde_json::Value::String(value))
        .map_err(settings_persistence_error)?;
    Ok(Json(state.settings_runtime_json()))
}

async fn save_worker_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let worker_id = request
        .get("worker")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let worker_config = request.get("config");

    let (saved_worker, saved_config) = if let (Some(worker_id), Some(worker_config)) =
        (worker_id, worker_config)
    {
        let worker_config = without_scope_binding_fields(worker_config.clone());
        reject_unknown_model_config_fields(&worker_config).map_err(ApiError::InvalidInput)?;
        NormalizedModelConfig::from_settings_value(&worker_config)
            .map_err(ApiError::InvalidInput)?;
        let mut workers = state
            .settings_store
            .get_section("workers")
            .as_object()
            .cloned()
            .unwrap_or_default();
        workers.insert(worker_id.to_string(), worker_config.clone());
        state
            .settings_store
            .set_section("workers", serde_json::Value::Object(workers))
            .map_err(settings_persistence_error)?;
        (Some(worker_id.to_string()), worker_config)
    } else {
        let workers = scoped_settings_section_request(&request)?;
        let Some(worker_configs) = workers.as_object() else {
            return Err(ApiError::InvalidInput("workers 配置必须是对象".to_string()));
        };
        for worker_config in worker_configs.values() {
            reject_unknown_model_config_fields(worker_config).map_err(ApiError::InvalidInput)?;
            NormalizedModelConfig::from_settings_value(worker_config)
                .map_err(ApiError::InvalidInput)?;
        }
        state
            .settings_store
            .set_section("workers", workers.clone())
            .map_err(settings_persistence_error)?;
        (None, workers)
    };
    Ok(Json(serde_json::json!({
        "saved": true,
        "worker": saved_worker,
        "config": saved_config,
    })))
}

async fn remove_worker_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let worker = request
        .get("worker")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    state
        .settings_store
        .remove_section_entry("workers", worker)
        .map_err(settings_persistence_error)?;
    Ok(Json(serde_json::json!({ "removed": true })))
}

async fn test_worker_connection(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let config = request
        .get("config")
        .cloned()
        .ok_or_else(|| ApiError::InvalidInput("worker 连接测试缺少 config".to_string()))?;
    probe_connection_response(&state, config).await
}

async fn save_orchestrator_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let config = orchestrator_connection_section_request(&request)?;
    let expected = request
        .get("expectedRevision")
        .and_then(Value::as_u64)
        .ok_or_else(|| ApiError::InvalidInput("保存数据源需要 expectedRevision".into()))?;
    let next_revision = expected
        .checked_add(1)
        .ok_or_else(|| ApiError::InvalidInput("数据源修订号已达到上限".into()))?;
    let mut saved = config;
    saved["revision"] = json!(next_revision);
    let changed = state
        .settings_store
        .update_section("orchestrator", |current| {
            let revision = current.get("revision").and_then(Value::as_u64).unwrap_or(0);
            if revision != expected {
                return false;
            }
            *current = saved.clone();
            true
        })
        .map_err(settings_persistence_error)?;
    if !changed {
        return Err(ApiError::InvalidInput(
            "数据源已被其他窗口修改，请重新加载后再保存".into(),
        ));
    }
    let config = saved;
    Ok(Json(serde_json::json!({ "saved": true, "config": config })))
}

async fn save_orchestrator_session_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let request = serde_json::from_value::<OrchestratorSessionConfigRequest>(request)
        .map_err(ApiError::invalid_request_body)?;
    let scope = session_scope::require_session_request_scope(
        &state,
        request.session_id.as_deref(),
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let override_config =
        save_orchestrator_session_override_for_session(&state, &scope.session_id, &request.config)?
            .unwrap_or_else(|| {
                state
                    .settings_store
                    .get_session_section(&scope.session_id, "orchestrator")
            });

    let effective_config = magi_conversation_runtime::main_model_sources::effective_config(
        &state.settings_store,
        &override_config,
    )
    .map_err(ApiError::InvalidInput)?;
    let capability =
        NormalizedModelConfig::from_settings_with_catalog(&state.settings_store, &effective_config)
            .ok()
            .and_then(|config| {
                config
                    .require_model()
                    .ok()
                    .map(|model| config.reasoning_capability(model))
            });
    Ok(Json(serde_json::json!({
        "orchestratorReasoningCapability": capability,
        "saved": true,
        "sessionId": scope.session_id.to_string(),
        "workspaceId": scope.workspace_id().map(|id| id.to_string()),
        "orchestratorSessionConfig": override_config,
        "effectiveOrchestratorConfig": effective_config,
    })))
}

async fn save_model_context_window(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("模型名称不能为空".to_string()))?;
    let context_window_tokens = request
        .get("contextWindowTokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| ApiError::InvalidInput("上下文窗口必须是整数".to_string()))?;
    let source_id = request
        .get("sourceId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if request.get("sourceId").is_some() && source_id.is_none() {
        return Err(ApiError::InvalidInput("sourceId 必须是非空字符串".into()));
    }
    let entries = if let Some(source_id) = source_id {
        magi_conversation_runtime::main_model_sources::effective_config(
            &state.settings_store,
            &json!({"sourceId": source_id, "model": model}),
        )
        .map_err(ApiError::InvalidInput)?;
        set_main_source_model_context_window(
            &state.settings_store,
            source_id,
            model,
            context_window_tokens,
        )
        .map_err(ApiError::InvalidInput)?
    } else {
        set_model_context_window(&state.settings_store, model, context_window_tokens)
            .map_err(ApiError::InvalidInput)?
    };
    let updated_at = UtcMillis::now();
    let _ = state.event_bus.publish(EventEnvelope::domain(
        EventId::unique("event-model-context-window-updated"),
        "model.context_window.updated",
        json!({
            "model": model,
            "sourceId": source_id,
            "contextWindowTokens": context_window_tokens,
            "modelContextWindows": entries,
            "updatedAt": updated_at.0,
        }),
    ));
    Ok(Json(json!({
        "saved": true,
        "model": model,
        "sourceId": source_id,
        "contextWindowTokens": context_window_tokens,
        "modelContextWindows": entries,
    })))
}

async fn test_orchestrator_connection(
    State(state): State<ApiState>,
    Json(config): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let normalized =
        NormalizedModelConfig::from_settings_with_catalog(&state.settings_store, &config)
            .map_err(ApiError::InvalidInput)?;
    normalized
        .require_base_url()
        .map_err(ApiError::InvalidInput)?;
    normalized
        .require_api_key()
        .map_err(ApiError::InvalidInput)?;
    if normalized.require_model().is_ok() {
        execute_connection_probe(&normalized).await?;
    } else {
        execute_model_catalog_probe(&normalized).await?;
    }
    Ok(Json(json!({
        "success": true,
        "message": "连接测试成功"
    })))
}

async fn save_auxiliary_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let config = model_settings_section_request(&request)?;
    state
        .settings_store
        .set_section("auxiliary", config.clone())
        .map_err(settings_persistence_error)?;
    Ok(Json(serde_json::json!({ "saved": true, "config": config })))
}

async fn test_auxiliary_connection(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    probe_connection_response(&state, request).await
}

async fn save_vision_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut config = model_settings_section_request(&request)?;
    let config_object = config
        .as_object_mut()
        .ok_or_else(|| ApiError::InvalidInput("识图模型配置必须是对象".to_string()))?;
    config_object
        .entry("contextWindowTokens".to_string())
        .or_insert_with(|| Value::from(DEFAULT_VISION_CONTEXT_WINDOW));
    validate_vision_model_settings(&config).map_err(ApiError::InvalidInput)?;
    let model = config
        .get("model")
        .and_then(Value::as_str)
        .expect("识图模型配置校验后必须包含 model");
    let context_window_tokens = config
        .get("contextWindowTokens")
        .and_then(Value::as_u64)
        .expect("识图模型配置校验后必须包含 contextWindowTokens");
    let context_windows =
        model_context_windows_with_update(&state.settings_store, model, context_window_tokens)
            .map_err(ApiError::InvalidInput)?;
    state
        .settings_store
        .apply_section_changes(
            [
                (VISION_MODEL_SECTION.to_string(), config.clone()),
                (
                    MODEL_CONTEXT_WINDOWS_SECTION.to_string(),
                    Value::Object(context_windows.clone()),
                ),
            ],
            Vec::<String>::new(),
        )
        .map_err(settings_persistence_error)?;
    Ok(Json(serde_json::json!({
        "saved": true,
        "config": config,
        "modelContextWindows": context_windows,
    })))
}

async fn test_vision_connection(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    probe_connection_response(&state, request).await
}

fn image_generation_section_request(request: &Value) -> Result<Value, ApiError> {
    let config = model_settings_section_request(request)?;
    let normalized =
        NormalizedModelConfig::from_settings_value(&config).map_err(ApiError::InvalidInput)?;
    normalized
        .require_base_url()
        .map_err(ApiError::InvalidInput)?;
    normalized
        .require_api_key()
        .map_err(ApiError::InvalidInput)?;
    normalized.require_model().map_err(ApiError::InvalidInput)?;

    let mut canonical = Map::new();
    for field in ["baseUrl", "apiKey", "model", "apiProtocol"] {
        if let Some(value) = config.get(field).cloned() {
            canonical.insert(field.to_string(), value);
        }
    }
    Ok(Value::Object(canonical))
}

async fn save_image_generation_config(
    State(state): State<ApiState>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let config = image_generation_section_request(&request)?;
    state
        .settings_store
        .set_section("imageGeneration", config.clone())
        .map_err(settings_persistence_error)?;
    Ok(Json(json!({ "saved": true, "config": config })))
}

async fn test_image_generation_connection(
    State(_state): State<ApiState>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let config = image_generation_section_request(&request)?;
    let normalized =
        NormalizedModelConfig::from_settings_value(&config).map_err(ApiError::InvalidInput)?;
    let client = normalized
        .to_http_image_generation_client()
        .map_err(ApiError::InvalidInput)?;
    let generated = tokio::task::spawn_blocking(move || {
        client.generate(BridgeImageGenerationRequest {
            prompt: "A simple blue square on a white background".to_string(),
            size: "1024x1024".to_string(),
            quality: None,
        })
    })
    .await
    .map_err(|error| {
        tracing::warn!(error = %error, "image generation connection probe thread failed");
        ApiError::InvalidInput("图片生成连接测试失败".to_string())
    })?
    .map_err(image_generation_probe_error)?;
    Ok(Json(json!({
        "success": true,
        "message": "图片生成测试成功",
        "mediaType": generated.media_type,
        "bytes": generated.bytes.len(),
    })))
}

fn image_generation_probe_error(error: BridgeClientError) -> ApiError {
    let raw = error.to_string();
    tracing::warn!(error = %raw, "image generation connection probe failed");
    if let Some(status) = error.http_status() {
        return ApiError::InvalidInput(model_http_status_error_message(status).to_string());
    }
    match error.layer() {
        Some(BridgeErrorLayer::Transport) => {
            ApiError::InvalidInput("图片生成服务连接失败".to_string())
        }
        Some(BridgeErrorLayer::Protocol) => {
            ApiError::InvalidInput("图片生成服务响应格式异常".to_string())
        }
        Some(BridgeErrorLayer::RemoteBusiness) | None => {
            ApiError::InvalidInput("图片生成服务暂不可用".to_string())
        }
    }
}

async fn save_user_rules(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .settings_store
        .set_section("userRules", scoped_settings_section_request(&request)?)
        .map_err(settings_persistence_error)?;
    Ok(Json(serde_json::json!({ "saved": true })))
}

/// 安全防护审计记录的分页读取：最新的在前，`before` 是上一页最后一条的 sequence。
/// 账本只存规则命中的摘要（工具、决策、命中规则），不含命令参数。
const SAFEGUARD_AUDIT_DEFAULT_PAGE: usize = 50;
const SAFEGUARD_AUDIT_MAX_PAGE: usize = 100;

async fn list_safeguard_audit(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let limit = match query.get("limit") {
        Some(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=SAFEGUARD_AUDIT_MAX_PAGE).contains(value))
            .ok_or_else(|| {
                ApiError::InvalidInput(format!(
                    "limit 必须是 1 到 {SAFEGUARD_AUDIT_MAX_PAGE} 之间的整数"
                ))
            })?,
        None => SAFEGUARD_AUDIT_DEFAULT_PAGE,
    };
    let before = match query.get("before") {
        Some(raw) => Some(
            raw.parse::<u64>()
                .map_err(|_| ApiError::InvalidInput("before 必须是账本序号".to_string()))?,
        ),
        None => None,
    };

    let (total, mut page) = state.event_bus.with_audit_usage_ledger(|ledger| {
        let matching = || crate::safeguard_audit::visible_entries(ledger);
        let total = matching().count();
        let page = matching()
            .rev()
            .filter(|entry| before.is_none_or(|before| entry.sequence < before))
            .take(limit + 1)
            .cloned()
            .collect::<Vec<_>>();
        (total, page)
    });
    let has_more = page.len() > limit;
    page.truncate(limit);
    let next_before = if has_more {
        page.last().map(|entry| entry.sequence)
    } else {
        None
    };
    let entries = page
        .iter()
        .map(|entry| {
            let session_id = entry.context.session_id.as_ref();
            let session_title = session_id
                .and_then(|id| state.session_store.session(id))
                .map(|session| session.title);
            json!({
                "eventId": entry.event_id,
                "sequence": entry.sequence,
                "occurredAt": entry.occurred_at.0,
                "toolName": entry.payload.get("tool_name").cloned().unwrap_or(Value::Null),
                "decision": entry.payload.get("decision").cloned().unwrap_or(Value::Null),
                "accessProfile": entry.payload.get("access_profile").cloned().unwrap_or(Value::Null),
                "matchedRules": entry
                    .payload
                    .get("matched_rules")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
                "sessionId": session_id.map(|id| id.as_str()),
                "sessionTitle": session_title,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "total": total,
        "entries": entries,
        "nextBefore": next_before,
    })))
}

/// 删除审计记录：`{ "eventIds": [...] }` 删除所选，`{ "all": true }` 清空当前全部。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteSafeguardAuditRequest {
    #[serde(default, rename = "eventIds")]
    event_ids: Option<Vec<String>>,
    #[serde(default)]
    all: bool,
}

async fn delete_safeguard_audit(
    State(state): State<ApiState>,
    Json(request): Json<DeleteSafeguardAuditRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let selection = match (request.event_ids, request.all) {
        (Some(ids), false) if !ids.is_empty() => crate::safeguard_audit::AuditSelection::Ids(ids),
        (None, true) => crate::safeguard_audit::AuditSelection::All,
        _ => {
            return Err(ApiError::InvalidInput(
                "必须且只能指定 eventIds（非空）或 all=true 之一".to_string(),
            ));
        }
    };
    let removed = crate::safeguard_audit::remove(&state.event_bus, selection);
    Ok(Json(json!({
        "removed": removed,
        "total": crate::safeguard_audit::visible_count(&state.event_bus),
    })))
}

async fn save_safeguard_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut normalized =
        crate::state::normalize_safeguard_config_value(scoped_settings_section_request(&request)?);
    let expected_revision = normalized
        .get("revision")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| ApiError::InvalidInput("安全防护配置缺少 revision".to_string()))?;
    let current = state.settings_store.get_section("safeguardConfig");
    let current_revision = current
        .get("revision")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if expected_revision != current_revision {
        return Err(ApiError::Conflict(format!(
            "安全防护配置已被其他窗口更新，请刷新后重试（expected={expected_revision}, actual={current_revision}）"
        )));
    }
    let next_revision = current_revision.saturating_add(1);
    normalized["revision"] = serde_json::Value::from(next_revision);
    state
        .settings_store
        .set_section("safeguardConfig", normalized.clone())
        .map_err(settings_persistence_error)?;
    state.event_bus.publish(
        EventEnvelope::domain(
            EventId::unique("event-safeguard-config-updated"),
            "settings.safeguard.updated",
            serde_json::json!({
                "revision": next_revision,
                "rule_count": normalized
                    .get("rules")
                    .and_then(serde_json::Value::as_array)
                    .map_or(0, Vec::len),
            }),
        )
        .with_context(EventContext::default()),
    );
    Ok(Json(serde_json::json!({
        "saved": true,
        "revision": next_revision,
        "config": normalized,
    })))
}

async fn list_role_templates(
    State(state): State<ApiState>,
    Query(_query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    Json(json!({
        "templates": role_templates_for_registry(state.agent_role_registry.as_ref())
    }))
}

fn role_error(error: magi_agent_role::AgentRoleError) -> ApiError {
    match error {
        magi_agent_role::AgentRoleError::Conflict(message) => ApiError::Conflict(message),
        magi_agent_role::AgentRoleError::InvalidDefinition(message)
        | magi_agent_role::AgentRoleError::Parse { message, .. } => ApiError::InvalidInput(message),
        magi_agent_role::AgentRoleError::Io { path, source } => ApiError::InternalAssemblyError(
            format!("角色文件持久化失败: {}: {source}", path.display()),
        ),
        magi_agent_role::AgentRoleError::InvalidId { path, file_stem } => {
            ApiError::InvalidInput(format!(
                "角色文件 {} 的 ID 无效（文件名 {}）",
                path.display(),
                file_stem
            ))
        }
    }
}

fn strict_string_field(value: &Value, name: &str, label: &str) -> Result<String, ApiError> {
    let Some(raw) = value.get(name) else {
        return Ok(String::new());
    };
    let Some(raw) = raw.as_str() else {
        return Err(ApiError::InvalidInput(format!("{label} 必须是字符串")));
    };
    Ok(raw.trim().to_string())
}

fn strict_string_list_field(
    value: &Value,
    name: &str,
    label: &str,
) -> Result<Vec<String>, ApiError> {
    let Some(raw) = value.get(name) else {
        return Ok(Vec::new());
    };
    let Some(items) = raw.as_array() else {
        return Err(ApiError::InvalidInput(format!("{label} 必须是字符串数组")));
    };
    let mut values = Vec::with_capacity(items.len());
    for item in items {
        let Some(value) = item.as_str().map(|value| value.trim().to_string()) else {
            return Err(ApiError::InvalidInput(format!(
                "{label} 的每个元素必须是字符串"
            )));
        };
        if !value.is_empty() {
            values.push(value);
        }
    }
    Ok(values)
}

fn strict_object_field(value: &Value, name: &str, label: &str) -> Result<Value, ApiError> {
    let Some(raw) = value.get(name) else {
        return Ok(json!({}));
    };
    if !raw.is_object() {
        return Err(ApiError::InvalidInput(format!("{label} 必须是对象")));
    }
    Ok(raw.clone())
}

fn strict_optional_u32_field(
    value: &Value,
    name: &str,
    label: &str,
) -> Result<Option<u32>, ApiError> {
    let Some(raw) = value.get(name) else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let Some(number) = raw.as_u64() else {
        return Err(ApiError::InvalidInput(format!(
            "{label} 必须是非负整数或 null"
        )));
    };
    let number = u32::try_from(number)
        .map_err(|_| ApiError::InvalidInput(format!("{label} 超出允许范围")))?;
    Ok(Some(number))
}

fn strict_u32_field(value: &Value, name: &str, default: u32, label: &str) -> Result<u32, ApiError> {
    let Some(raw) = value.get(name) else {
        return Ok(default);
    };
    let Some(number) = raw.as_u64() else {
        return Err(ApiError::InvalidInput(format!("{label} 必须是非负整数")));
    };
    u32::try_from(number).map_err(|_| ApiError::InvalidInput(format!("{label} 超出允许范围")))
}

fn strict_u64_field(value: &Value, name: &str, default: u64, label: &str) -> Result<u64, ApiError> {
    let Some(raw) = value.get(name) else {
        return Ok(default);
    };
    raw.as_u64()
        .ok_or_else(|| ApiError::InvalidInput(format!("{label} 必须是非负整数")))
}

fn strict_optional_u64_field(
    value: &Value,
    name: &str,
    label: &str,
) -> Result<Option<u64>, ApiError> {
    let Some(raw) = value.get(name) else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    raw.as_u64()
        .map(Some)
        .ok_or_else(|| ApiError::InvalidInput(format!("{label} 必须是非负整数或 null")))
}

fn strict_optional_string_field(
    value: &Value,
    name: &str,
    label: &str,
) -> Result<Option<String>, ApiError> {
    let Some(raw) = value.get(name) else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    raw.as_str()
        .map(|value| value.trim().to_string())
        .map(Some)
        .ok_or_else(|| ApiError::InvalidInput(format!("{label} 必须是字符串或 null")))
}

fn strict_bool_field(
    value: &Value,
    name: &str,
    default: bool,
    label: &str,
) -> Result<bool, ApiError> {
    let Some(raw) = value.get(name) else {
        return Ok(default);
    };
    raw.as_bool()
        .ok_or_else(|| ApiError::InvalidInput(format!("{label} 必须是布尔值")))
}

fn role_from_value(value: &Value) -> Result<magi_agent_role::AgentRole, ApiError> {
    let id = strict_string_field(value, "id", "角色 id")?;
    let supported_kinds_value = value
        .get("supportedKinds")
        .cloned()
        .unwrap_or_else(|| json!(["local_agent"]));
    let supported_kinds = serde_json::from_value(supported_kinds_value)
        .map_err(|error| ApiError::InvalidInput(format!("supportedKinds 无效: {error}")))?;
    let profile = strict_object_field(value, "profile", "profile")?;
    let default_ui = strict_object_field(value, "defaultUI", "defaultUI")?;
    let parallelism_limit =
        strict_optional_u32_field(value, "parallelismLimit", "parallelismLimit")?;
    let coordinator_mode = strict_bool_field(value, "coordinatorMode", false, "coordinatorMode")?;
    let version = strict_u32_field(value, "version", 1, "version")?;
    let role_revision = strict_u64_field(value, "roleRevision", 1, "roleRevision")?;
    magi_agent_role::normalize_role(magi_agent_role::AgentRole {
        id,
        system_prompt: strict_string_field(value, "systemPrompt", "systemPrompt")?,
        display_name: strict_string_field(value, "displayName", "displayName")?,
        description: strict_string_field(value, "description", "description")?,
        supported_kinds,
        parallelism_limit,
        coordinator_mode,
        version,
        role_revision,
        role: strict_string_field(&profile, "role", "profile.role")?,
        focus: strict_string_list_field(&profile, "focus", "profile.focus")?,
        constraints: strict_string_list_field(&profile, "constraints", "profile.constraints")?,
        output_preferences: strict_string_list_field(
            &profile,
            "outputPreferences",
            "profile.outputPreferences",
        )?,
        ownerships: strict_string_list_field(value, "ownerships", "ownerships")?,
        insight_preferences: strict_string_list_field(
            value,
            "insightPreferences",
            "insightPreferences",
        )?,
        capabilities: strict_string_list_field(value, "capabilities", "capabilities")?,
        color_token: strict_string_field(&default_ui, "colorToken", "defaultUI.colorToken")?,
        icon: strict_string_field(&default_ui, "icon", "defaultUI.icon")?,
    })
    .map_err(ApiError::InvalidInput)
}

fn role_response(state: &ApiState, role: magi_agent_role::AgentRole) -> Value {
    let template = role_templates_for_registry(state.agent_role_registry.as_ref())
        .into_iter()
        .find(|template| {
            template.get("templateId").and_then(Value::as_str) == Some(role.id.as_str())
        })
        .unwrap_or(Value::Null);
    json!({
        "role": template,
        "registryRevision": state.agent_role_registry.registry_revision(),
        "agents": resolve_registry_agents(state),
    })
}

async fn upsert_role(
    State(state): State<ApiState>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let _configuration_guard = state
        .role_configuration_lock
        .lock()
        .map_err(|_| ApiError::InternalAssemblyError("角色配置事务锁已损坏".to_string()))?;
    let role_value = request
        .get("role")
        .ok_or_else(|| ApiError::InvalidInput("角色配置必须放在 role 字段中".to_string()))?;
    let role = role_from_value(role_value)?;
    let expected =
        strict_optional_u64_field(&request, "expectedRoleRevision", "expectedRoleRevision")?;
    let saved = state
        .agent_role_registry
        .save_user_role(role, expected)
        .map_err(role_error)?;
    Ok(Json(role_response(&state, saved)))
}

async fn delete_role(
    State(state): State<ApiState>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let _configuration_guard = state
        .role_configuration_lock
        .lock()
        .map_err(|_| ApiError::InternalAssemblyError("角色配置事务锁已损坏".to_string()))?;
    let role_id = strict_string_field(&request, "templateId", "角色 ID")?;
    magi_agent_role::validate_role_id(&role_id).map_err(ApiError::InvalidInput)?;
    let role = state
        .agent_role_registry
        .get(&role_id)
        .ok_or_else(|| ApiError::not_found("角色不存在", &role_id))?;
    if state.agent_role_registry.is_builtin(&role_id) {
        return Err(ApiError::Conflict(format!(
            "角色 {} 是系统内置角色，不能删除",
            role_id
        )));
    }
    let expected =
        strict_optional_u64_field(&request, "expectedRoleRevision", "expectedRoleRevision")?;
    if expected != Some(role.role_revision) {
        return Err(ApiError::Conflict(format!(
            "角色 {} 已被其他窗口修改，请刷新后重试",
            role_id
        )));
    }
    if let Some(task_store) = state.task_store()
        && let Some((_, lease_id)) =
            task_store
                .collect_all_active_leases()
                .into_iter()
                .find(|(_, lease_id)| {
                    task_store
                        .get_lease(lease_id)
                        .is_some_and(|lease| lease.role == role_id)
                })
    {
        return Err(ApiError::Conflict(format!(
            "角色 {} 正在被运行中的 Worker 使用（租约 {}），请等待任务结束后再删除",
            role_id, lease_id
        )));
    }
    // 先把删除前像写入状态根事务日志，再把绑定和角色文件作为一个可恢复操作提交。
    // daemon 崩溃在任一中间点时，启动恢复会按日志恢复完整删除前像。
    let previous_agents = state.settings_store.get("agents");
    let role_file_path = state
        .agent_role_registry
        .user_role_dir()
        .join(format!("{role_id}.md"));
    let previous_role_file = match std::fs::read_to_string(&role_file_path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(ApiError::InternalAssemblyError(format!(
                "读取角色删除前像失败: {}: {error}",
                role_file_path.display()
            )));
        }
    };
    let state_root = state.runtime_persistence().map(|persistence| {
        persistence
            .state_root()
            .expect("runtime persistence 必须包含 state root")
    });
    if let Some(state_root) = state_root {
        crate::state::prepare_role_delete_transaction(
            state_root,
            &role_id,
            previous_role_file,
            previous_agents.clone(),
        )
        .map_err(ApiError::InternalAssemblyError)?;
    }

    let template_ids = template_ids_for_registry(state.agent_role_registry.as_ref());
    let order_map = template_order_map_for_registry(state.agent_role_registry.as_ref());
    let mut overrides = load_agent_overrides(&state, &template_ids, &order_map);
    overrides
        .retain(|entry| entry.get("templateId").and_then(Value::as_str) != Some(role_id.as_str()));
    if let Err(error) = state
        .settings_store
        .set_section("agents", Value::Array(overrides))
    {
        if let Some(state_root) = state_root {
            let _ =
                crate::state::recover_role_delete_transaction(state_root, &state.settings_store);
        }
        return Err(settings_persistence_error(error));
    }
    if let Err(error) = state
        .agent_role_registry
        .delete_user_role(&role_id, expected)
    {
        let restore_result = if let Some(state_root) = state_root {
            crate::state::recover_role_delete_transaction(state_root, &state.settings_store)
        } else {
            match previous_agents {
                Some(previous_agents) => state
                    .settings_store
                    .set_section("agents", previous_agents)
                    .map_err(|error| error.to_string()),
                None => state
                    .settings_store
                    .remove_section("agents")
                    .map_err(|error| error.to_string()),
            }
        };
        if let Err(restore_error) = restore_result {
            return Err(ApiError::InternalAssemblyError(format!(
                "角色删除失败且绑定恢复失败: {}; 请重新加载设置后处理角色 {}（恢复错误: {restore_error}）",
                error, role_id
            )));
        }
        return Err(role_error(error));
    }
    if let Some(state_root) = state_root {
        if let Err(error) = crate::state::mark_role_delete_transaction_committed(state_root) {
            let recovery =
                crate::state::recover_role_delete_transaction(state_root, &state.settings_store);
            let reload = state.agent_role_registry.reload_from_disk();
            return Err(ApiError::InternalAssemblyError(match (recovery, reload) {
                (Ok(()), Ok(_)) => format!("角色删除提交记录失败，已恢复删除前状态: {error}"),
                (recovery, reload) => format!(
                    "角色删除提交记录失败，且恢复失败: {error}; recovery={recovery:?}; reload={reload:?}"
                ),
            }));
        }
        if let Err(error) = crate::state::clear_role_delete_transaction(state_root) {
            tracing::warn!(%error, role_id = %role_id, "角色删除已提交，但事务日志清理失败，将在下次启动清理");
        }
    }
    Ok(Json(json!({
        "deleted": role_id,
        "registryRevision": state.agent_role_registry.registry_revision(),
        "templates": role_templates_for_registry(state.agent_role_registry.as_ref()),
        "agents": resolve_registry_agents(&state),
    })))
}

fn generated_role_id_seed(value: &str) -> String {
    let mut slug = String::new();
    let mut separator_pending = false;
    for character in value.trim().chars() {
        let character = character.to_ascii_lowercase();
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            if separator_pending && !slug.is_empty() {
                slug.push('-');
            }
            separator_pending = false;
            slug.push(character);
        } else {
            separator_pending = true;
        }
    }
    let slug = slug.chars().take(48).collect::<String>();
    if slug.is_empty() {
        "custom-role".to_string()
    } else {
        slug.trim_end_matches('-').to_string()
    }
}

fn generated_import_role_id(
    registry: &magi_agent_role::AgentRoleRegistry,
    role: &magi_agent_role::AgentRole,
) -> String {
    let source = if magi_agent_role::validate_role_id(role.id.trim()).is_ok() {
        role.id.trim()
    } else {
        role.display_name.trim()
    };
    let base = generated_role_id_seed(source);
    let copy_suffix = "-copy";
    let copy_prefix = base
        .chars()
        .take(64 - copy_suffix.len())
        .collect::<String>()
        .trim_end_matches('-')
        .to_string();
    let copy_base = format!("{copy_prefix}{copy_suffix}");
    if !registry.contains(&copy_base) {
        return copy_base;
    }

    let mut suffix = 2u64;
    loop {
        let suffix_text = format!("-{suffix}");
        let prefix = copy_base
            .chars()
            .take(64 - suffix_text.len())
            .collect::<String>()
            .trim_end_matches('-')
            .to_string();
        let candidate = format!("{prefix}{suffix_text}");
        if !registry.contains(&candidate) {
            return candidate;
        }
        suffix = suffix.saturating_add(1);
    }
}

async fn import_role(
    State(state): State<ApiState>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let _configuration_guard = state
        .role_configuration_lock
        .lock()
        .map_err(|_| ApiError::InternalAssemblyError("角色配置事务锁已损坏".to_string()))?;
    let content = request
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidInput("导入内容不能为空".to_string()))?;
    if content.chars().count() > 100_000 {
        return Err(ApiError::InvalidInput(
            "导入文件不能超过 100000 个字符".to_string(),
        ));
    }
    let mut role = magi_agent_role::parse_role_markdown(content)
        .map_err(ApiError::InvalidInput)
        .and_then(|role| magi_agent_role::normalize_role(role).map_err(ApiError::InvalidInput))?;
    let conflict = request
        .get("conflict")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| ApiError::InvalidInput("conflict 必须是字符串".to_string()))
        })
        .transpose()?
        .unwrap_or("reject");
    if !matches!(conflict, "reject" | "overwrite" | "rename") {
        return Err(ApiError::InvalidInput(
            "conflict 只支持 reject、overwrite、rename".to_string(),
        ));
    }
    let new_id = strict_optional_string_field(&request, "newId", "newId")?;
    if conflict == "rename" {
        role.id = new_id
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| generated_import_role_id(&state.agent_role_registry, &role));
        role.role_revision = 1;
    }
    let expected = if conflict == "overwrite" {
        match state.agent_role_registry.get(&role.id) {
            Some(existing) if state.agent_role_registry.is_user_defined(&role.id) => {
                Some(existing.role_revision)
            }
            Some(_) => {
                return Err(ApiError::Conflict(format!(
                    "角色 {} 是系统内置角色，不能覆盖",
                    role.id
                )));
            }
            None => {
                return Err(ApiError::Conflict(format!(
                    "角色 {} 不存在，不能使用覆盖导入；请选择拒绝或另存为",
                    role.id
                )));
            }
        }
    } else {
        None
    };
    let saved = state
        .agent_role_registry
        .save_user_role(role, expected)
        .map_err(role_error)?;
    Ok(Json(role_response(&state, saved)))
}

async fn export_role(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let role_id = query
        .get("templateId")
        .or_else(|| query.get("id"))
        .map(String::as_str)
        .unwrap_or("")
        .trim();
    let role = state
        .agent_role_registry
        .get(role_id)
        .ok_or_else(|| ApiError::not_found("角色不存在", role_id))?;
    Ok(Json(json!({
        "templateId": role.id,
        "fileName": format!("{}.md", role.id),
        "content": magi_agent_role::serialize_role_markdown(&role),
        "registryRevision": state.agent_role_registry.registry_revision(),
    })))
}

async fn list_engines(
    State(state): State<ApiState>,
    Query(_query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let engines = load_registry_engines(&state);
    Json(json!({ "engines": engines }))
}

async fn upsert_engine(
    State(state): State<ApiState>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    crate::model_config::upsert_engine(&state, &request).map(Json)
}

async fn remove_engine(
    State(state): State<ApiState>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    crate::model_config::remove_engine(&state, &request).map(Json)
}

async fn list_agents(
    State(state): State<ApiState>,
    Query(_query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let agents = resolve_registry_agents(&state);
    Json(json!({ "agents": agents }))
}

async fn upsert_agent(
    State(state): State<ApiState>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    crate::model_config::upsert_agent(&state, &request).map(Json)
}

async fn remove_agent(
    State(state): State<ApiState>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    crate::model_config::remove_agent(&state, &request).map(Json)
}

async fn main_model_catalog(State(state): State<ApiState>) -> Result<Json<Value>, ApiError> {
    magi_conversation_runtime::main_model_sources::catalog(&state.settings_store)
        .map(Json)
        .map_err(ApiError::InvalidInput)
}

async fn fetch_models(
    State(state): State<ApiState>,
    Json(request): Json<FetchModelsRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let FetchModelsRequest {
        config,
        target,
        workspace_id,
    } = request;

    let request_config = config.clone();
    let provider_result = parse_fetch_models_config(FetchModelsRequest {
        config: config.clone(),
        target: target.clone(),
        workspace_id: workspace_id.clone(),
    });
    let mut model_capabilities = serde_json::Map::new();
    let mut catalog_display_names = serde_json::Map::new();
    let models = match provider_result {
        Ok((config, _)) => match fetch_model_catalog_for_config(&config).await {
            Ok(catalog) if !catalog.models.is_empty() => {
                persist_model_reasoning_catalog(&state, &config, &catalog)?;
                let models = catalog.models;
                // Re-read the persisted catalog through the same resolver used for execution.
                let projection_config = request_config;
                let resolved = NormalizedModelConfig::from_settings_with_catalog(
                    &state.settings_store,
                    &projection_config,
                )
                .map_err(ApiError::InvalidInput)?;
                for model in &models {
                    if let Some(name) = catalog.display_names.get(model) {
                        catalog_display_names.insert(model.clone(), json!(name));
                    }
                    model_capabilities
                        .insert(model.clone(), json!(resolved.reasoning_capability(model)));
                }

                models
            }

            Ok(_) => {
                return Err(ApiError::InvalidInput(
                    "该 API 不支持模型列表查询，请手动填写模型名".to_string(),
                ));
            }

            Err(error) => return Err(error),
        },

        Err(error) => return Err(error),
    };

    if models.is_empty() {
        return Err(ApiError::InvalidInput(
            "该 API 不支持模型列表查询，请手动填写模型名".to_string(),
        ));
    }
    let now = UtcMillis::now();
    Ok(Json(serde_json::json!({
        "success": true,
        "target": target,
        "models": models,
        "modelDisplayNames": catalog_display_names,
        "modelCapabilities": model_capabilities,
        "requestedAt": now.0,
    })))
}

async fn model_reasoning_capability(
    State(state): State<ApiState>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let raw = request
        .get("config")
        .ok_or_else(|| ApiError::InvalidInput("缺少模型配置".into()))?;
    let resolved_source;
    let raw = if raw.get("sourceId").is_some() {
        resolved_source = magi_conversation_runtime::main_model_sources::effective_config(
            &state.settings_store,
            &json!({
                "sourceId":raw.get("sourceId"), "model":request.get("model"),
            }),
        )
        .map_err(ApiError::InvalidInput)?;
        &resolved_source
    } else {
        reject_unknown_model_config_fields(raw).map_err(ApiError::InvalidInput)?;
        raw
    };

    let config = NormalizedModelConfig::from_settings_with_catalog(&state.settings_store, raw)
        .map_err(ApiError::InvalidInput)?;
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let known_in_catalog = state
        .settings_store
        .get_section("modelReasoningCatalog")
        .get(config.reasoning_catalog_key())
        .and_then(|entries| entries.get(model))
        .is_some();
    if !model.is_empty() && !known_in_catalog {
        let catalog = fetch_model_catalog_for_config(&config).await?;
        persist_model_reasoning_catalog(&state, &config, &catalog)?;
        let refreshed =
            NormalizedModelConfig::from_settings_with_catalog(&state.settings_store, raw)
                .map_err(ApiError::InvalidInput)?;
        return Ok(Json(json!(refreshed.reasoning_capability(model))));
    }
    Ok(Json(json!(config.reasoning_capability(model))))
}

async fn execution_stats(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = parse_optional_query_string(&query, "sessionId");
    if let Some(id) = &session_id
        && state
            .session_store
            .session(&magi_core::SessionId::new(id.clone()))
            .is_none()
    {
        return Err(ApiError::session_not_found(id));
    }
    let mut authority = state.event_bus.with_audit_usage_ledger(|ledger| {
        usage_authority_from_usage_entries_filtered(&ledger.usage_entries, session_id.as_deref())
    });
    let (version, updated_at, totals, bindings, models) = if let Some(id) = &session_id {
        let snapshot = authority.get_session_snapshot("all", id);
        (
            snapshot.version,
            snapshot.updated_at,
            snapshot.totals,
            snapshot.by_execution_binding,
            snapshot.by_model_identity,
        )
    } else {
        let snapshot = authority.get_workspace_snapshot("all");
        (
            snapshot.version,
            snapshot.updated_at,
            snapshot.totals,
            snapshot.by_execution_binding,
            snapshot.by_model_identity,
        )
    };
    Ok(Json(serde_json::json!({
        "version": version,
        "lastAppliedLedgerSeq": version,
        "updatedAt": updated_at,
        "totals": usage_totals_json(&totals),
        "items": bindings.into_iter().map(usage_binding_item_json).collect::<Vec<_>>(),
        "models": models.into_iter().map(usage_model_item_json).collect::<Vec<_>>(),
    })))
}

/// 执行统计只计入最后一次 `usage.stats.reset` 之后的用量条目；账本本身只追加，重置不删除历史。
fn usage_authority_from_usage_entries_filtered(
    usage_entries: &[AuditUsageLedgerEntry],
    session_id: Option<&str>,
) -> UsageAuthority {
    let reset_through = usage_entries
        .iter()
        .rev()
        .find(|entry| entry.event_type == USAGE_STATS_RESET_EVENT_TYPE)
        .map_or(0, |entry| entry.sequence);
    let entries =
        &usage_entries[usage_entries.partition_point(|entry| entry.sequence <= reset_through)..];
    let mut authority = UsageAuthority::new();
    for entry in entries {
        if entry.event_type != "model.usage.recorded" {
            continue;
        }
        let Ok(mut input) = serde_json::from_value::<UsageCallRecordInput>(entry.payload.clone())
        else {
            tracing::warn!(
                event_id = entry.event_id,
                "模型用量账本条目无法解析，已跳过"
            );
            continue;
        };
        if session_id.is_some_and(|id| input.owner.session_id() != Some(id)) {
            continue;
        }
        if input.event_id.is_none() {
            input.event_id = Some(entry.event_id.clone());
        }
        if input.timestamp.is_none() {
            input.timestamp = Some(entry.occurred_at.0);
        }
        input.workspace_id = Some("all".to_string());
        authority.append_call_record(input);
    }

    authority
}

fn usage_binding_item_json(
    binding: magi_usage_authority::UsageBindingSnapshot,
) -> serde_json::Value {
    serde_json::json!({
        "templateId": binding.template_id,
        "pluginId": binding.plugin_id,
        "engineId": binding.engine_id,
        "bindingRevision": binding.binding_revision,
        "role": binding.role,
        "displayName": binding.display_name.as_deref().unwrap_or(&binding.template_id),
        "provider": binding.provider,
        "declaredModelSpec": binding.declared_model_spec,
        "resolvedModel": binding.resolved_model,
        "modelIdentityKey": binding.model_identity_key,
        "llmCallCount": binding.totals.llm_call_count,
        "assignmentCount": binding.totals.assignment_count,
        "successCount": binding.totals.success_count,
        "failureCount": binding.totals.failure_count,
        "totalTokens": binding.totals.total_tokens,
        "netInputTokens": binding.totals.net_input_tokens,
        "netOutputTokens": binding.totals.net_output_tokens,
        "tokenObservedCallCount": binding.totals.token_observed_call_count,
        "cache": usage_cache_json(&binding.totals),
        "performance": binding.totals.performance,
        "inputTokens": binding.totals.net_input_tokens + binding.totals.cache_read_tokens + binding.totals.cache_write_tokens,
    })
}

fn usage_totals_json(totals: &UsageTotals) -> serde_json::Value {
    serde_json::json!({
        "llmCallCount": totals.llm_call_count,
        "assignmentCount": totals.assignment_count,
        "turnCount": totals.turn_count,
        "totalTokens": totals.total_tokens,
        "netInputTokens": totals.net_input_tokens,
        "netOutputTokens": totals.net_output_tokens,
        "successCount": totals.success_count,
        "failureCount": totals.failure_count,
        "tokenObservedCallCount": totals.token_observed_call_count,
        "cache": usage_cache_json(totals),
        "performance": totals.performance,
        "inputTokens": totals.net_input_tokens + totals.cache_read_tokens + totals.cache_write_tokens,
    })
}

fn usage_cache_json(totals: &UsageTotals) -> serde_json::Value {
    serde_json::json!({
        "observedCallCount": totals.cache_observed_call_count,
        "hitCallCount": totals.cache_hit_call_count,
        "inputTokens": totals.cache_observed_input_tokens,
        "readTokens": totals.cache_read_tokens,
        "writeTokens": totals.cache_write_tokens,
        "writeObservedCallCount": totals.cache_write_observed_call_count,
        "tokenHitRate": (totals.cache_observed_input_tokens > 0).then(||
            totals.cache_read_tokens as f64 / totals.cache_observed_input_tokens as f64),
        "requestHitRate": (totals.cache_observed_call_count > 0).then(||
            totals.cache_hit_call_count as f64 / totals.cache_observed_call_count as f64),
    })
}

fn usage_model_item_json(model: UsageModelSnapshot) -> serde_json::Value {
    serde_json::json!({
        "modelIdentityKey": model.model_identity_key,
        "provider": model.provider,
        "declaredModelSpec": model.declared_model_spec,
        "resolvedModel": model.resolved_model,
        "baseUrlFingerprint": model.base_url_fingerprint,
        "reasoningEffort": model.reasoning_effort,
        "totals": usage_totals_json(&model.totals),
    })
}

async fn reset_stats(
    State(state): State<ApiState>,
    Json(_request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.event_bus.publish(EventEnvelope::usage(
        EventId::new(format!("usage-stats-reset-{}", UtcMillis::now().0)),
        USAGE_STATS_RESET_EVENT_TYPE,
        json!({}),
    ));
    state
        .event_bus
        .refresh_audit_usage_ledger_persistence()
        .map_err(|error| ApiError::internal_assembly("重置执行统计失败", error))?;
    Ok(Json(serde_json::json!({ "reset": true })))
}

#[cfg(test)]
mod tests;
