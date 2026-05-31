use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_bridge_client::HttpModelBridgeProtocol;
use magi_core::{SessionId, UtcMillis, WorkspaceId};
use magi_tool_runtime::ToolExecutionContext;
use magi_usage_authority::{
    SessionSummary, UsageAuthority, UsageCallRecordInput, UsageModelSnapshot, UsageTotals,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::{errors::ApiError, model_config::NormalizedModelConfig, state::ApiState};

fn unwrap_settings_section_request(request: &serde_json::Value) -> serde_json::Value {
    request
        .get("config")
        .or_else(|| request.get("data"))
        .cloned()
        .unwrap_or_else(|| request.clone())
}

fn strip_scope_binding_fields(mut request: serde_json::Value) -> serde_json::Value {
    if let Some(object) = request.as_object_mut() {
        for key in [
            "workspaceId",
            "workspace_id",
            "workspacePath",
            "workspace_path",
            "sessionId",
            "session_id",
        ] {
            object.remove(key);
        }
    }
    request
}

fn scoped_settings_section_request(request: &serde_json::Value) -> serde_json::Value {
    strip_scope_binding_fields(unwrap_settings_section_request(request))
}

fn parse_optional_query_string(
    query: &HashMap<String, String>,
    camel_key: &str,
    snake_key: &str,
) -> Option<String> {
    query
        .get(camel_key)
        .or_else(|| query.get(snake_key))
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FetchModelsRequest {
    config: Value,
    target: String,
}

fn parse_fetch_models_config(
    request: FetchModelsRequest,
) -> Result<(NormalizedModelConfig, String), ApiError> {
    let config = NormalizedModelConfig::from_settings_value(&request.config, "openai");
    config.require_base_url().map_err(ApiError::InvalidInput)?;
    config.require_api_key().map_err(ApiError::InvalidInput)?;
    config
        .require_models_listable()
        .map_err(ApiError::InvalidInput)?;
    Ok((config, request.target))
}

fn parse_model_ids(payload: &Value) -> Vec<String> {
    fn extract_name(entry: &Value) -> Option<String> {
        entry
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| entry.get("name").and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }

    let mut models = Vec::new();
    if let Some(data) = payload.get("data").and_then(Value::as_array) {
        for entry in data {
            if let Some(name) = extract_name(entry) {
                models.push(name);
            }
        }
    }
    if models.is_empty()
        && let Some(entries) = payload.get("models").and_then(Value::as_array)
    {
        for entry in entries {
            match entry {
                Value::String(value) => {
                    let trimmed = value.trim();
                    if !trimmed.is_empty() {
                        models.push(trimmed.to_string());
                    }
                }
                _ => {
                    if let Some(name) = extract_name(entry) {
                        models.push(name);
                    }
                }
            }
        }
    }
    models.sort();
    models.dedup();
    models
}

fn parse_connection_probe_config(request: Value) -> Result<NormalizedModelConfig, ApiError> {
    let config = unwrap_settings_section_request(&request);
    let normalized = NormalizedModelConfig::from_settings_value(&config, "openai");
    normalized
        .require_base_url()
        .map_err(ApiError::InvalidInput)?;
    normalized
        .require_api_key()
        .map_err(ApiError::InvalidInput)?;
    normalized.require_model().map_err(ApiError::InvalidInput)?;
    Ok(normalized)
}

async fn execute_connection_probe(
    config: &NormalizedModelConfig,
) -> Result<(u16, Value), ApiError> {
    // 探针不再维护双轨 body builder：直接借用 HttpModelBridgeClient::build_probe_request
    // 作为唯一事实源，保证 reasoning_effort、Anthropic thinking 等字段与生产链路完全一致。
    let client = config
        .to_http_model_client("__probe__")
        .ok_or_else(|| ApiError::InvalidInput("模型配置缺少 baseUrl".to_string()))?;
    let (url, body, extra_headers) = client
        .build_probe_request()
        .map_err(|error| ApiError::InvalidInput(format!("构造连接测试请求失败: {error}")))?;

    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    for (name, value) in extra_headers {
        let header_name = HeaderName::try_from(name.as_str())
            .map_err(|_| ApiError::InvalidInput(format!("探针请求包含非法 header 名: {name}")))?;
        let header_value = HeaderValue::from_str(&value)
            .map_err(|_| ApiError::InvalidInput(format!("探针请求包含非法 header 值: {name}")))?;
        headers.insert(header_name, header_value);
    }

    let response = reqwest::Client::new()
        .post(url)
        .headers(headers)
        .json(&body)
        .send()
        .await
        .map_err(|error| ApiError::InvalidInput(format!("连接测试失败: {error}")))?;
    let status = response.status().as_u16();
    let payload: Value = response
        .json()
        .await
        .map_err(|error| ApiError::InvalidInput(format!("解析连接测试响应失败: {error}")))?;

    Ok((status, payload))
}

fn extract_remote_error_message(payload: &Value) -> Option<String> {
    payload
        .get("error")
        .and_then(|value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| value.as_str())
        })
        .or_else(|| payload.get("message").and_then(Value::as_str))
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(ToOwned::to_owned)
}

async fn probe_connection_response(request: Value) -> Result<Json<Value>, ApiError> {
    let config = parse_connection_probe_config(request)?;
    let (status, payload) = execute_connection_probe(&config).await?;
    if !(200..300).contains(&status) {
        let message = extract_remote_error_message(&payload)
            .unwrap_or_else(|| format!("连接测试失败: HTTP {status}"));
        return Err(ApiError::InvalidInput(message));
    }

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
            "/settings/orchestrator/test",
            post(test_orchestrator_connection),
        )
        .route("/settings/auxiliary/save", post(save_auxiliary_config))
        .route("/settings/auxiliary/test", post(test_auxiliary_connection))
        .route("/settings/user-rules/save", post(save_user_rules))
        .route("/settings/safeguard/save", post(save_safeguard_config))
        .route(
            "/settings/registry/role-templates",
            get(list_role_templates),
        )
        .route("/settings/registry/engines", get(list_engines))
        .route("/settings/registry/engines/upsert", post(upsert_engine))
        .route("/settings/registry/engines/remove", post(remove_engine))
        .route("/settings/registry/agents", get(list_agents))
        .route("/settings/registry/agents/upsert", post(upsert_agent))
        .route("/settings/registry/agents/remove", post(remove_agent))
        .route("/settings/models/fetch", post(fetch_models))
        .route("/settings/stats/session", get(session_stats))
        .route("/settings/stats/reset", post(reset_stats))
}

pub(crate) fn builtin_role_templates() -> Vec<Value> {
    // 这里只暴露可被 agent_spawn 派发的代理角色。
    // coordinator 是主线编排的内部身份，由主模型承接，不进入用户可配置角色列表。
    vec![
        json!({
            "templateId": "executor",
            "displayName": "Executor",
            "description": "负责把已定义的 WorkPackage / Action 落地执行（写代码、改配置、跑构建）",
            "i18n": {
                "displayNameKey": "roleTemplate.executor.displayName",
                "descriptionKey": "roleTemplate.executor.description",
            },
            "defaultUI": { "colorToken": "agent-executor", "icon": "tool" },
            "profile": {
                "role": "executor",
                "focus": ["implementation", "build", "runtime", "contracts"],
                "constraints": ["preserve-authoritative-state", "avoid-duplication"],
                "outputPreferences": ["diff", "runtime-impact", "follow-up"],
            },
            "ownerships": ["implementation"],
            "insightPreferences": ["decision", "contract", "risk"],
        }),
        json!({
            "templateId": "explorer",
            "displayName": "Explorer",
            "description": "负责搜索代码库、分析失败原因、定位根因与梳理调用链",
            "i18n": {
                "displayNameKey": "roleTemplate.explorer.displayName",
                "descriptionKey": "roleTemplate.explorer.description",
            },
            "defaultUI": { "colorToken": "agent-explorer", "icon": "bug" },
            "profile": {
                "role": "explorer",
                "focus": ["root-cause", "investigation", "evidence", "trace"],
                "constraints": ["fix-at-source", "no-patchy-workarounds"],
                "outputPreferences": ["root-cause", "evidence", "next-step"],
            },
            "ownerships": ["investigation"],
            "insightPreferences": ["decision", "risk", "constraint"],
        }),
        json!({
            "templateId": "reviewer",
            "displayName": "Reviewer",
            "description": "负责风险识别、回归扫描与交付质量把关",
            "i18n": {
                "displayNameKey": "roleTemplate.reviewer.displayName",
                "descriptionKey": "roleTemplate.reviewer.description",
            },
            "defaultUI": { "colorToken": "agent-reviewer", "icon": "shield" },
            "profile": {
                "role": "reviewer",
                "focus": ["risk", "regression", "consistency", "delivery"],
                "constraints": ["no-new-second-truth", "flag-hidden-risk"],
                "outputPreferences": ["findings", "severity", "follow-up"],
            },
            "ownerships": ["quality"],
            "insightPreferences": ["risk", "constraint", "decision"],
        }),
        json!({
            "templateId": "tester",
            "displayName": "Tester",
            "description": "负责验证链路、场景覆盖与失败复现",
            "i18n": {
                "displayNameKey": "roleTemplate.tester.displayName",
                "descriptionKey": "roleTemplate.tester.description",
            },
            "defaultUI": { "colorToken": "agent-tester", "icon": "check-circle" },
            "profile": {
                "role": "tester",
                "focus": ["verification", "coverage", "smoke", "repro"],
                "constraints": ["prefer-real-paths", "keep-signal-high"],
                "outputPreferences": ["steps", "result", "follow-up"],
            },
            "ownerships": ["verification"],
            "insightPreferences": ["risk", "constraint"],
        }),
        json!({
            "templateId": "architect",
            "displayName": "Architect",
            "description": "负责结构裁决、边界治理与长期演进方向",
            "i18n": {
                "displayNameKey": "roleTemplate.architect.displayName",
                "descriptionKey": "roleTemplate.architect.description",
            },
            "defaultUI": { "colorToken": "agent-architect", "icon": "grid" },
            "profile": {
                "role": "architect",
                "focus": ["architecture", "boundaries", "tradeoffs", "evolution"],
                "constraints": ["avoid-duplicate-systems", "prefer-clear-boundaries"],
                "outputPreferences": ["decision", "tradeoff", "follow-up"],
            },
            "ownerships": ["architecture"],
            "insightPreferences": ["decision", "constraint", "risk"],
        }),
    ]
}

fn builtin_template_ids() -> HashSet<String> {
    builtin_role_templates()
        .iter()
        .filter_map(|template| {
            template
                .get("templateId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn builtin_template_order_map() -> HashMap<String, usize> {
    builtin_role_templates()
        .into_iter()
        .enumerate()
        .filter_map(|(index, template)| {
            template
                .get("templateId")
                .and_then(Value::as_str)
                .map(|template_id| (template_id.to_string(), index))
        })
        .collect()
}

fn default_agent_binding(template_id: &str, order: usize) -> Value {
    json!({
        "templateId": template_id,
        "engineId": "",
        "bindingRevision": 0,
        "order": order,
    })
}

fn normalize_engine_entry(entry: &Value) -> Option<Value> {
    let raw = entry.get("engine").unwrap_or(entry);
    let engine_id = raw
        .get("id")
        .or_else(|| raw.get("engineId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let mut normalized = Map::new();
    normalized.insert("id".to_string(), Value::String(engine_id.to_string()));
    normalized.insert(
        "displayName".to_string(),
        Value::String(
            raw.get("displayName")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(engine_id)
                .to_string(),
        ),
    );
    normalized.insert(
        "llm".to_string(),
        raw.get("llm")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new())),
    );
    if let Some(runtime) = raw.get("runtime").cloned() {
        normalized.insert("runtime".to_string(), runtime);
    }
    Some(Value::Object(normalized))
}

fn normalize_engine_entries(raw_engines: &Value) -> Vec<Value> {
    let mut normalized = Vec::new();
    let mut seen = HashSet::new();
    let Some(entries) = raw_engines.as_array() else {
        return normalized;
    };
    for entry in entries {
        let Some(normalized_entry) = normalize_engine_entry(entry) else {
            continue;
        };
        let Some(engine_id) = normalized_entry
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        if seen.insert(engine_id) {
            normalized.push(normalized_entry);
        }
    }
    normalized
}

fn normalize_worker_model_config_entries(raw_workers: &Value) -> HashMap<String, Value> {
    let mut normalized = HashMap::new();
    let Some(workers) = raw_workers.as_object() else {
        return normalized;
    };
    for (worker_id, worker_config) in workers {
        let worker_id = worker_id.trim();
        if worker_id.is_empty() {
            continue;
        }
        normalized.insert(worker_id.to_string(), worker_config.clone());
    }
    normalized
}

fn align_engine_llm_with_worker_configs(
    engines: &mut [Value],
    worker_configs: &HashMap<String, Value>,
) {
    for engine in engines {
        let Some(engine_id) = engine
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        let Some(worker_config) = worker_configs.get(&engine_id) else {
            continue;
        };
        if let Some(engine_object) = engine.as_object_mut() {
            engine_object.insert("llm".to_string(), worker_config.clone());
        }
    }
}

pub(crate) fn load_registry_engines(state: &ApiState) -> Vec<Value> {
    let raw_engines = state.settings_store.get_section("engines");
    let mut normalized = normalize_engine_entries(&raw_engines);
    let worker_configs =
        normalize_worker_model_config_entries(&state.settings_store.get_section("workers"));
    align_engine_llm_with_worker_configs(&mut normalized, &worker_configs);
    if raw_engines != Value::Array(normalized.clone()) {
        state
            .settings_store
            .set_section("engines", Value::Array(normalized.clone()));
    }
    normalized
}

fn normalize_agent_override_entry(
    entry: &Value,
    template_ids: &HashSet<String>,
    order_map: &HashMap<String, usize>,
) -> Option<Value> {
    let raw = entry.get("agent").unwrap_or(entry);
    let template_id = raw
        .get("templateId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if !template_ids.contains(template_id) {
        return None;
    }
    // `engineId` 是「继承 vs 显式」的唯一字段：空串 = 继承编排模型，非空 = 显式绑定到 engine。
    // 历史载荷里的 `modelSource` 枚举（"orchestrator"/"engine"）在 normalize 阶段直接丢弃，
    // 不再作为二级判定项——避免「modelSource=engine 但 engineId 空」这类矛盾态。
    let engine_id = raw
        .get("engineId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let order = order_map.get(template_id).copied().unwrap_or(0);
    let binding_revision = raw
        .get("bindingRevision")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let mut normalized = Map::new();
    normalized.insert(
        "templateId".to_string(),
        Value::String(template_id.to_string()),
    );
    normalized.insert("engineId".to_string(), Value::String(engine_id));
    normalized.insert("bindingRevision".to_string(), Value::from(binding_revision));
    normalized.insert("order".to_string(), Value::from(order as u64));

    if let Some(ui_overrides) = raw.get("uiOverrides").cloned() {
        normalized.insert("uiOverrides".to_string(), ui_overrides);
    }
    if let Some(profile_overrides) = raw.get("profileOverrides").cloned() {
        normalized.insert("profileOverrides".to_string(), profile_overrides);
    }

    Some(Value::Object(normalized))
}

fn is_default_agent_override(override_entry: &Value) -> bool {
    let engine_id = override_entry
        .get("engineId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let has_ui_overrides = override_entry
        .get("uiOverrides")
        .is_some_and(|value| !value.is_null());
    let has_profile_overrides = override_entry
        .get("profileOverrides")
        .is_some_and(|value| !value.is_null());
    engine_id.is_empty() && !has_ui_overrides && !has_profile_overrides
}

fn load_agent_overrides(
    state: &ApiState,
    template_ids: &HashSet<String>,
    order_map: &HashMap<String, usize>,
) -> Vec<Value> {
    let raw_agents = state.settings_store.get_section("agents");
    let mut normalized = Vec::new();
    let mut seen = HashSet::new();
    if let Some(entries) = raw_agents.as_array() {
        for entry in entries {
            let Some(normalized_entry) =
                normalize_agent_override_entry(entry, template_ids, order_map)
            else {
                continue;
            };
            let Some(template_id) = normalized_entry
                .get("templateId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
            else {
                continue;
            };
            if !seen.insert(template_id) || is_default_agent_override(&normalized_entry) {
                continue;
            }
            normalized.push(normalized_entry);
        }
    }
    if raw_agents != Value::Array(normalized.clone()) {
        state
            .settings_store
            .set_section("agents", Value::Array(normalized.clone()));
    }
    normalized
}

pub(crate) fn resolve_registry_agents(state: &ApiState) -> Vec<Value> {
    let templates = builtin_role_templates();
    let template_ids = builtin_template_ids();
    let order_map = builtin_template_order_map();
    let overrides = load_agent_overrides(state, &template_ids, &order_map);
    let override_map: HashMap<String, Value> = overrides
        .into_iter()
        .filter_map(|entry| {
            let template_id = entry
                .get("templateId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)?;
            Some((template_id, entry))
        })
        .collect();

    templates
        .iter()
        .enumerate()
        .filter_map(|(index, template)| {
            let template_id = template.get("templateId")?.as_str()?;
            let default_binding = default_agent_binding(template_id, index);
            let resolved = if let Some(override_entry) = override_map.get(template_id) {
                let mut merged = default_binding.as_object()?.clone();
                if let Some(override_object) = override_entry.as_object() {
                    for (key, value) in override_object {
                        merged.insert(key.clone(), value.clone());
                    }
                }
                Value::Object(merged)
            } else {
                default_binding
            };
            Some(resolved)
        })
        .collect()
}

pub(crate) fn registered_role_template_ids(state: &ApiState) -> Vec<String> {
    resolve_registry_agents(state)
        .into_iter()
        .filter_map(|entry| {
            entry
                .get("templateId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect()
}

async fn settings_bootstrap(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let hydrate_mcp_servers = query
        .get("scope")
        .map(|value| value.trim())
        .is_none_or(|scope| scope != "core");
    let tool_context = settings_bootstrap_tool_context(&query);
    let workspace_id =
        parse_optional_query_string(&query, "workspaceId", "workspace_id").unwrap_or_default();
    let workspace_path =
        parse_optional_query_string(&query, "workspacePath", "workspace_path").unwrap_or_default();
    let mut snapshot = state.settings_snapshot_json_with_mcp_hydration_and_tool_context(
        hydrate_mcp_servers,
        &tool_context,
    );
    if let Some(object) = snapshot.as_object_mut() {
        object.insert("workspaceId".to_string(), Value::String(workspace_id));
        object.insert("workspacePath".to_string(), Value::String(workspace_path));
    }
    Json(snapshot)
}

fn settings_bootstrap_tool_context(query: &HashMap<String, String>) -> ToolExecutionContext {
    ToolExecutionContext {
        session_id: parse_optional_query_string(query, "sessionId", "session_id")
            .map(SessionId::new),
        workspace_id: parse_optional_query_string(query, "workspaceId", "workspace_id")
            .map(WorkspaceId::new),
        working_directory: parse_optional_query_string(query, "workspacePath", "workspace_path")
            .map(PathBuf::from),
        ..ToolExecutionContext::default()
    }
}

async fn runtime_status(State(state): State<ApiState>) -> Json<serde_json::Value> {
    Json(state.runtime_status_json())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSettingRequest {
    key: String,
    value: serde_json::Value,
}

async fn update_setting(
    State(state): State<ApiState>,
    Json(request): Json<UpdateSettingRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .settings_store
        .set(&request.key, request.value.clone());
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

    if let (Some(worker_id), Some(worker_config)) = (worker_id, worker_config) {
        let mut workers = state
            .settings_store
            .get_section("workers")
            .as_object()
            .cloned()
            .unwrap_or_default();
        workers.insert(worker_id.to_string(), worker_config.clone());
        state
            .settings_store
            .set_section("workers", serde_json::Value::Object(workers));
    } else {
        state
            .settings_store
            .set_section("workers", scoped_settings_section_request(&request));
    }
    Ok(Json(serde_json::json!({ "saved": true })))
}

async fn remove_worker_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let worker = request
        .get("worker")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    state.settings_store.remove_section_entry("workers", worker);
    Ok(Json(serde_json::json!({ "removed": true })))
}

async fn test_worker_connection(
    State(_state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    probe_connection_response(request).await
}

async fn save_orchestrator_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .settings_store
        .set_section("orchestrator", scoped_settings_section_request(&request));
    Ok(Json(serde_json::json!({ "saved": true })))
}

async fn test_orchestrator_connection(
    State(_state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    probe_connection_response(request).await
}

async fn save_auxiliary_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .settings_store
        .set_section("auxiliary", scoped_settings_section_request(&request));
    Ok(Json(serde_json::json!({ "saved": true })))
}

async fn test_auxiliary_connection(
    State(_state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    probe_connection_response(request).await
}

async fn save_user_rules(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .settings_store
        .set_section("userRules", scoped_settings_section_request(&request));
    Ok(Json(serde_json::json!({ "saved": true })))
}

async fn save_safeguard_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let normalized =
        crate::state::normalize_safeguard_config_value(scoped_settings_section_request(&request));
    state
        .settings_store
        .set_section("safeguardConfig", normalized);
    Ok(Json(serde_json::json!({ "saved": true })))
}

async fn list_role_templates(
    State(_state): State<ApiState>,
    Query(_query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    Json(json!({ "templates": builtin_role_templates() }))
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
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let normalized = normalize_engine_entry(&request)
        .ok_or_else(|| ApiError::InvalidInput("引擎配置缺少有效的 id".to_string()))?;
    let engine_id = normalized
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidInput("引擎配置缺少有效的 id".to_string()))?
        .to_string();
    let mut engines = load_registry_engines(&state);
    if let Some(position) = engines.iter().position(|entry| {
        entry
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|value| value == engine_id)
    }) {
        engines[position] = normalized;
    } else {
        engines.push(normalized);
    }
    state
        .settings_store
        .set_section("engines", Value::Array(engines.clone()));
    Ok(Json(json!({ "engines": engines })))
}

async fn remove_engine(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let engine_id = request
        .get("engineId")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    state
        .settings_store
        .remove_array_entry("engines", "engineId", engine_id);
    let mut engines = load_registry_engines(&state);
    engines.retain(|entry| {
        entry
            .get("id")
            .and_then(Value::as_str)
            .is_none_or(|value| value != engine_id)
    });
    state
        .settings_store
        .set_section("engines", Value::Array(engines.clone()));
    Ok(Json(json!({ "engines": engines })))
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
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let template_ids = builtin_template_ids();
    let order_map = builtin_template_order_map();
    let normalized = normalize_agent_override_entry(&request, &template_ids, &order_map)
        .ok_or_else(|| ApiError::InvalidInput("角色绑定缺少有效的 templateId".to_string()))?;
    let template_id = normalized
        .get("templateId")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidInput("角色绑定缺少有效的 templateId".to_string()))?
        .to_string();
    let engine_id = normalized
        .get("engineId")
        .and_then(Value::as_str)
        .unwrap_or("");

    // engineId 非空 ⇒ 显式绑定，必须能在 engines 段里查到。
    // 空串则代表「继承编排模型」，没有 engine 概念，直接进 overrides 收敛。
    if !engine_id.is_empty() {
        let engines = load_registry_engines(&state);
        let engine_exists = engines.iter().any(|entry| {
            entry
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|value| value == engine_id)
        });
        if !engine_exists {
            return Err(ApiError::conflict("角色绑定的引擎不存在", &engine_id));
        }
    }

    let mut overrides = load_agent_overrides(&state, &template_ids, &order_map);
    overrides.retain(|entry| {
        entry
            .get("templateId")
            .and_then(Value::as_str)
            .is_none_or(|value| value != template_id)
    });
    if !is_default_agent_override(&normalized) {
        overrides.push(normalized);
    }
    state
        .settings_store
        .set_section("agents", Value::Array(overrides));

    Ok(Json(json!({ "agents": resolve_registry_agents(&state) })))
}

async fn remove_agent(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let template_id = request
        .get("templateId")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let template_ids = builtin_template_ids();
    let order_map = builtin_template_order_map();
    let mut overrides = load_agent_overrides(&state, &template_ids, &order_map);
    overrides.retain(|entry| {
        entry
            .get("templateId")
            .and_then(Value::as_str)
            .is_none_or(|value| value != template_id)
    });
    state
        .settings_store
        .set_section("agents", Value::Array(overrides));
    Ok(Json(json!({ "agents": resolve_registry_agents(&state) })))
}

async fn fetch_models(
    State(_state): State<ApiState>,
    Json(request): Json<FetchModelsRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (config, target) = parse_fetch_models_config(request)?;
    let url = config.models_list_url().map_err(ApiError::InvalidInput)?;
    let now = UtcMillis::now();
    let api_key = config.require_api_key().map_err(ApiError::InvalidInput)?;
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    // 认证 header 按推断协议选：OpenAI 兼容端点用 Bearer，Anthropic 兼容端点用 x-api-key + anthropic-version。
    // /v1/models 路径在两边都是合法端点，只有 header 是认证语义的分歧点。
    match config.inferred_protocol() {
        HttpModelBridgeProtocol::ChatCompletions => {
            let auth_value = HeaderValue::from_str(&format!("Bearer {}", api_key))
                .map_err(|_| ApiError::InvalidInput("apiKey 包含非法字符".to_string()))?;
            headers.insert(AUTHORIZATION, auth_value);
        }
        HttpModelBridgeProtocol::AnthropicMessages => {
            let key_value = HeaderValue::from_str(&api_key)
                .map_err(|_| ApiError::InvalidInput("apiKey 包含非法字符".to_string()))?;
            headers.insert(HeaderName::from_static("x-api-key"), key_value);
            headers.insert(
                HeaderName::from_static("anthropic-version"),
                HeaderValue::from_static("2023-06-01"),
            );
        }
    }

    let response = reqwest::Client::new()
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(|error| ApiError::InvalidInput(format!("获取模型列表失败: {error}")))?;
    let status = response.status();
    let payload: Value = response
        .json()
        .await
        .map_err(|error| ApiError::InvalidInput(format!("解析模型列表响应失败: {error}")))?;
    if !status.is_success() {
        let message = payload
            .get("error")
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("模型列表请求失败: HTTP {}", status.as_u16()));
        return Err(ApiError::InvalidInput(message));
    }

    let models = parse_model_ids(&payload);
    if models.is_empty() {
        return Err(ApiError::InvalidInput(
            "该 API 不支持模型列表查询，请手动填写模型名".to_string(),
        ));
    }

    Ok(Json(serde_json::json!({
        "success": true,
        "target": target,
        "models": models,
        "requestedAt": now.0,
    })))
}

async fn session_stats(
    State(state): State<ApiState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let workspace_id =
        parse_optional_query_string(&query, "workspaceId", "workspace_id").unwrap_or_default();
    let session_id = parse_optional_query_string(&query, "sessionId", "session_id");
    let mut authority = usage_authority_from_model_usage_ledger(&state);
    if let Some(session_id) = session_id.as_deref() {
        let snapshot = authority.get_session_snapshot(&workspace_id, session_id);
        Json(serde_json::json!({
            "scope": "session",
            "workspaceId": snapshot.workspace_id,
            "sessionId": snapshot.session_id,
            "version": snapshot.version,
            "lastAppliedLedgerSeq": snapshot.last_applied_ledger_seq,
            "updatedAt": snapshot.updated_at,
            "totals": usage_totals_json(&snapshot.totals),
            "items": snapshot.by_execution_binding.into_iter().map(usage_binding_item_json).collect::<Vec<_>>(),
            "models": snapshot.by_model_identity.into_iter().map(usage_model_item_json).collect::<Vec<_>>(),
        }))
    } else {
        let snapshot = authority.get_workspace_snapshot(&workspace_id);
        Json(serde_json::json!({
            "scope": "workspace",
            "workspaceId": snapshot.workspace_id,
            "sessionId": serde_json::Value::Null,
            "version": snapshot.version,
            "lastAppliedLedgerSeq": snapshot.version,
            "updatedAt": snapshot.updated_at,
            "totals": usage_totals_json(&snapshot.totals),
            "items": snapshot.by_execution_binding.into_iter().map(usage_binding_item_json).collect::<Vec<_>>(),
            "models": snapshot.by_model_identity.into_iter().map(usage_model_item_json).collect::<Vec<_>>(),
            "sessions": snapshot.by_session.into_iter().map(usage_session_summary_json).collect::<Vec<_>>(),
        }))
    }
}

fn usage_authority_from_model_usage_ledger(state: &ApiState) -> UsageAuthority {
    let ledger = state.event_bus.audit_usage_ledger_snapshot();
    let mut authority = UsageAuthority::new();
    for entry in ledger.usage_entries {
        if entry.event_type != "model.usage.recorded" {
            continue;
        }
        let Ok(mut input) = serde_json::from_value::<UsageCallRecordInput>(entry.payload) else {
            tracing::warn!(
                event_id = entry.event_id,
                "模型用量账本条目无法解析，已跳过"
            );
            continue;
        };
        if input.event_id.is_none() {
            input.event_id = Some(entry.event_id);
        }
        if input.timestamp.is_none() {
            input.timestamp = Some(entry.occurred_at.0);
        }
        authority.append_call_record(input);
    }
    authority
}

fn usage_binding_item_json(
    binding: magi_usage_authority::UsageBindingSnapshot,
) -> serde_json::Value {
    serde_json::json!({
        "templateId": binding.template_id,
        "engineId": binding.engine_id,
        "bindingRevision": binding.binding_revision,
        "role": binding.role,
        "displayName": binding.template_id,
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

fn usage_session_summary_json(summary: SessionSummary) -> serde_json::Value {
    serde_json::json!({
        "sessionId": summary.session_id,
        "version": summary.version,
        "updatedAt": summary.updated_at,
        "totals": usage_totals_json(&summary.totals),
    })
}

async fn reset_stats(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = request
        .get("workspaceId")
        .or_else(|| request.get("workspace_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default-workspace");
    let session_id = request
        .get("sessionId")
        .or_else(|| request.get("session_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut snapshot = state.event_bus.audit_usage_ledger_snapshot();
    snapshot.usage_entries.retain(|entry| {
        if entry.event_type != "model.usage.recorded" {
            return true;
        }
        let same_workspace = entry
            .context
            .workspace_id
            .as_ref()
            .map(ToString::to_string)
            .is_none_or(|value| value == workspace_id);
        let same_session = session_id.is_none_or(|session_id| {
            entry
                .context
                .session_id
                .as_ref()
                .map(ToString::to_string)
                .is_some_and(|value| value == session_id)
        });
        !(same_workspace && same_session)
    });
    state.event_bus.import_audit_usage_ledger_snapshot(snapshot);
    state
        .event_bus
        .refresh_audit_usage_ledger_persistence()
        .map_err(|error| ApiError::internal_assembly("重置执行统计失败", error))?;
    Ok(Json(serde_json::json!({ "reset": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{EventId, SessionId, WorkspaceId};
    use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_snapshot::SnapshotManager;
    use magi_tool_runtime::ToolRegistry;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;

    fn test_state() -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let governance = Arc::new(GovernanceService::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let session_store = Arc::new(SessionStore::default());
        let snapshot_manager = Arc::new(SnapshotManager::new());
        let runtime_capability_dependency_provider =
            crate::state::build_runtime_capability_dependency_provider(
                snapshot_manager.clone(),
                workspace_store.clone(),
                true,
            );
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus))
            .with_runtime_capability_dependency_provider(runtime_capability_dependency_provider);
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

    async fn anthropic_probe_stub(headers: HeaderMap, Json(payload): Json<Value>) -> Json<Value> {
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
        assert_eq!(payload["messages"][0]["content"], json!("ping"));
        Json(json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "text", "text": "pong" }],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1
            }
        }))
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

        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "provider": "anthropic",
                "baseUrl": base_url,
                "apiKey": "test-key",
                "model": "claude-sonnet-test",
                "urlMode": "standard"
            }),
            "openai",
        );
        let (status, payload) = execute_connection_probe(&config)
            .await
            .expect("anthropic probe should succeed");

        assert_eq!(status, 200);
        assert_eq!(payload["content"][0]["text"], json!("pong"));
        server.abort();
    }

    #[tokio::test]
    async fn settings_bootstrap_returns_frontend_contract_sections() {
        let state = test_state();
        let session_id = SessionId::new("session-empty-contract");

        let bootstrap = settings_bootstrap(
            State(state),
            Query(HashMap::from([
                ("sessionId".to_string(), session_id.as_str().to_string()),
                ("workspaceId".to_string(), "workspace-contract".to_string()),
                (
                    "workspacePath".to_string(),
                    "/tmp/magi-settings-contract".to_string(),
                ),
            ])),
        )
        .await
        .0;
        let object = bootstrap
            .as_object()
            .expect("settings bootstrap should be an object");

        for key in [
            "workerConfigs",
            "orchestratorConfig",
            "auxiliaryConfig",
            "userRulesConfig",
            "skillsConfig",
            "safeguardConfig",
            "workerStatuses",
            "runtimeSettings",
        ] {
            assert!(bootstrap[key].is_object(), "{key} should be an object");
        }
        assert_eq!(bootstrap["workspaceId"], json!("workspace-contract"));
        assert_eq!(
            bootstrap["workspacePath"],
            json!("/tmp/magi-settings-contract")
        );
        for key in [
            "repositories",
            "mcpServers",
            "builtinTools",
            "capabilityDependencies",
            "roleTemplates",
            "registryEngines",
            "registryAgents",
        ] {
            assert!(bootstrap[key].is_array(), "{key} should be an array");
        }
        let builtin_tools = bootstrap["builtinTools"]
            .as_array()
            .expect("builtin tools should be an array");
        assert_eq!(builtin_tools.len(), 30);
        let builtin_names: Vec<_> = builtin_tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect();
        assert_eq!(
            builtin_names,
            vec![
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
                "diagram_render",
                "knowledge_query",
                "code_symbols",
                "tool_catalog",
                "agent_spawn",
                "agent_wait",
                "todo_write",
                "memory_write",
                "mission_charter_write",
                "plan_write",
                "kg_write",
                "validation_record",
                "checkpoint_create",
                "human_checkpoint_request",
            ],
            "bootstrap must expose one canonical public builtin surface in a stable order"
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
        let knowledge_query = builtin_tools
            .iter()
            .find(|tool| tool["name"] == serde_json::json!("knowledge_query"))
            .expect("knowledge_query should be exposed");
        assert_eq!(
            knowledge_query["runtimeStatus"],
            serde_json::json!("unavailable"),
            "settings bootstrap must surface runtime health from tool_catalog"
        );
        assert!(
            knowledge_query["runtimeWarnings"]
                .as_array()
                .is_some_and(|warnings| !warnings.is_empty()),
            "unavailable builtin tools should include actionable runtime warnings"
        );
        let capability_dependencies = bootstrap["capabilityDependencies"]
            .as_array()
            .expect("capability dependencies should be an array");
        assert_eq!(capability_dependencies.len(), 7);
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
            serde_json::json!(["knowledge_query", "search_semantic", "code_symbols"])
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
            capability_dependencies[1]["workspaceId"],
            serde_json::json!("workspace-contract")
        );
        assert_eq!(
            capability_dependencies[2]["name"],
            serde_json::json!("agent_role_registry")
        );
        assert_eq!(
            capability_dependencies[3]["name"],
            serde_json::json!("skill_runtime")
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
            capability_dependencies[4]["readyCount"],
            serde_json::json!(0)
        );
        assert!(
            capability_dependencies[4].get("ready_count").is_none(),
            "settings bootstrap should expose external dependency counts in frontend camelCase"
        );
        assert_eq!(
            capability_dependencies[5]["name"],
            serde_json::json!("context_runtime")
        );
        assert_eq!(
            capability_dependencies[5]["status"],
            serde_json::json!("ready")
        );
        assert_eq!(
            capability_dependencies[5]["sessionId"],
            serde_json::json!("session-empty-contract")
        );
        assert_eq!(
            capability_dependencies[6]["name"],
            serde_json::json!("file_snapshot")
        );
        assert_eq!(
            capability_dependencies[6]["status"],
            serde_json::json!("not_ready"),
            "snapshot dependency should be visible even before the lazy snapshot session starts"
        );
        assert_eq!(
            capability_dependencies[6]["sessionId"],
            serde_json::json!("session-empty-contract")
        );
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
            bootstrap["safeguardConfig"]["rules"]
                .as_array()
                .expect("safeguard rules should be an array")
                .len()
                > 0
        );
        assert!(!object.contains_key("userRules"));
        assert!(!object.contains_key("engines"));
        assert!(!object.contains_key("agents"));
    }

    #[tokio::test]
    async fn settings_bootstrap_preserves_anthropic_model_providers() {
        let state = test_state();
        state.settings_store.set_section(
            "orchestrator",
            json!({
                "provider": "anthropic",
                "baseUrl": "https://api.anthropic.com",
                "model": "claude-sonnet-test",
                "urlMode": "standard"
            }),
        );
        state.settings_store.set_section(
            "workers",
            json!({
                "sonnet-worker": {
                    "provider": "anthropic",
                    "baseUrl": "https://api.anthropic.com",
                    "model": "claude-worker-test",
                    "urlMode": "standard"
                }
            }),
        );
        state.settings_store.set_section(
            "engines",
            json!([{
                "id": "sonnet-worker",
                "displayName": "Sonnet Worker",
                "llm": {
                    "provider": "anthropic",
                    "baseUrl": "https://api.anthropic.com",
                    "model": "claude-worker-test",
                    "urlMode": "standard"
                }
            }]),
        );

        let bootstrap = settings_bootstrap(State(state), Query(HashMap::new()))
            .await
            .0;

        assert_eq!(
            bootstrap["orchestratorConfig"]["provider"],
            json!("anthropic")
        );
        assert_eq!(
            bootstrap["workerConfigs"]["sonnet-worker"]["provider"],
            json!("anthropic")
        );
        assert_eq!(
            bootstrap["registryEngines"][0]["llm"]["provider"],
            json!("anthropic")
        );
    }

    #[tokio::test]
    async fn settings_bootstrap_aligns_registry_engine_llm_from_worker_config() {
        let state = test_state();
        state.settings_store.set_section(
            "workers",
            json!({
                "sonnet-worker": {
                    "provider": "anthropic",
                    "baseUrl": "https://api.anthropic.com",
                    "model": "claude-worker-test",
                    "urlMode": "standard"
                }
            }),
        );
        state.settings_store.set_section(
            "engines",
            json!([{
                "id": "sonnet-worker",
                "displayName": "Sonnet Worker",
                "llm": {
                    "provider": "openai",
                    "baseUrl": "https://api.openai.com",
                    "model": "stale-openai-model",
                    "urlMode": "standard"
                }
            }]),
        );

        let bootstrap = settings_bootstrap(State(state.clone()), Query(HashMap::new()))
            .await
            .0;

        assert_eq!(
            bootstrap["registryEngines"][0]["llm"]["provider"],
            json!("anthropic")
        );
        assert_eq!(
            bootstrap["registryEngines"][0]["llm"]["model"],
            json!("claude-worker-test")
        );
        let persisted_engines = state.settings_store.get_section("engines");
        assert_eq!(persisted_engines[0]["llm"]["provider"], json!("anthropic"));
    }

    #[test]
    fn normalize_engine_entry_preserves_supported_model_provider() {
        let normalized = normalize_engine_entry(&json!({
            "id": "sonnet-4-5",
            "displayName": "sonnet-4.5",
            "llm": {
                "provider": "anthropic",
                "baseUrl": "http://localhost:8317/",
                "model": "kiro-claude-sonnet-4-5-agentic",
                "urlMode": "standard"
            }
        }))
        .expect("engine should normalize");

        assert_eq!(normalized["llm"]["provider"], json!("anthropic"));
        assert_eq!(
            normalized["llm"]["model"],
            json!("kiro-claude-sonnet-4-5-agentic")
        );
    }

    #[tokio::test]
    async fn settings_bootstrap_filters_mcp_servers_without_id() {
        let state = test_state();
        state.settings_store.set_section(
            "mcpServers",
            json!([
                { "name": "broken", "command": "npx", "enabled": false },
                { "server": { "serverId": "wrapped-server", "command": "npx", "enabled": false } },
                { "id": "valid-server", "command": "npx", "enabled": false },
                "invalid-entry"
            ]),
        );

        let bootstrap = settings_bootstrap(State(state), Query(HashMap::new()))
            .await
            .0;
        let servers = bootstrap["mcpServers"]
            .as_array()
            .expect("mcpServers should be an array");
        assert_eq!(servers.len(), 2);
        assert!(
            servers
                .iter()
                .all(|server| server["id"].as_str().is_some_and(|id| !id.is_empty())),
            "bootstrap must not expose MCP server entries without id"
        );
        assert!(servers.iter().any(|server| {
            server["id"] == json!("wrapped-server")
                && server["serverId"] == json!("wrapped-server")
                && server["name"] == json!("wrapped-server")
        }));
        assert!(servers.iter().any(|server| {
            server["id"] == json!("valid-server") && server["serverId"] == json!("valid-server")
        }));
    }

    #[tokio::test]
    async fn settings_bootstrap_core_scope_defers_mcp_hydration() {
        let state = test_state();
        let bootstrap = settings_bootstrap(
            State(state),
            Query(HashMap::from([("scope".to_string(), "core".to_string())])),
        )
        .await
        .0;

        assert_eq!(bootstrap["bootstrapScope"], json!("core"));
        assert_eq!(bootstrap["mcpServersHydrated"], json!(false));
    }

    #[test]
    fn scoped_settings_section_request_strips_all_scope_binding_fields() {
        let cleaned = scoped_settings_section_request(&json!({
            "config": {
                "provider": "openai",
                "workspaceId": "workspace-a",
                "workspace_id": "workspace-b",
                "workspacePath": "/tmp/a",
                "workspace_path": "/tmp/b",
                "sessionId": "session-a",
                "session_id": "session-b"
            }
        }));

        assert_eq!(cleaned["provider"], json!("openai"));
        for key in [
            "workspaceId",
            "workspace_id",
            "workspacePath",
            "workspace_path",
            "sessionId",
            "session_id",
        ] {
            assert!(
                cleaned.get(key).is_none(),
                "{key} should not be persisted in settings sections"
            );
        }
    }

    #[tokio::test]
    async fn save_orchestrator_config_ignores_session_scope_and_writes_global_main_model() {
        let state = test_state();
        let _ = save_orchestrator_config(
            State(state.clone()),
            Json(json!({
                "config": {
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-global",
                    "model": "global-main-model",
                    "sessionId": "session-a",
                    "workspaceId": "workspace-a"
                }
            })),
        )
        .await
        .expect("orchestrator config should save");

        let saved = state.settings_store.get_section("orchestrator");
        assert_eq!(saved["model"], json!("global-main-model"));
        assert!(saved.get("sessionId").is_none());
        assert!(saved.get("workspaceId").is_none());
        assert!(
            state
                .settings_store
                .get_session_section(&SessionId::new("session-a"), "orchestrator")
                .is_null(),
            "主模型不支持按 session 保存，保存接口只能写全局 orchestrator 段"
        );
    }

    #[test]
    fn fetch_models_config_allows_anthropic_compatible_provider() {
        // /v1/models 在 Anthropic 端点同样合法；standard 模式下协议由当前模型名识别。
        let (config, target) = parse_fetch_models_config(FetchModelsRequest {
            config: serde_json::json!({
                "baseUrl": "https://api.anthropic.com",
                "apiKey": "test-key",
                "model": "claude-sonnet-test",
                "urlMode": "standard"
            }),
            target: "orch".to_string(),
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
                "urlMode": "standard"
            }),
            target: "orch".to_string(),
        })
        .expect("openai-compatible provider can list models");

        assert_eq!(config.provider(), "openai");
        assert_eq!(
            config.require_base_url().expect("baseUrl"),
            "http://127.0.0.1:8320/v1"
        );
        assert_eq!(config.require_api_key().expect("apiKey"), "test-key");
        config
            .require_models_listable()
            .expect("standard url mode can list models");
        assert_eq!(target, "orch");
    }

    #[test]
    fn fetch_models_config_rejects_full_url_mode() {
        let error = parse_fetch_models_config(FetchModelsRequest {
            config: serde_json::json!({
                "baseUrl": "http://127.0.0.1:8320/v1/chat/completions",
                "apiKey": "test-key",
                "urlMode": "full"
            }),
            target: "orch".to_string(),
        })
        .expect_err("full path mode has no canonical models endpoint");

        match error {
            ApiError::InvalidInput(message) => {
                assert!(message.contains("完整路径模式下不支持自动获取模型列表"));
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_stats_returns_frontend_stats_contract() {
        let state = test_state();
        let payload = session_stats(
            State(state),
            Query(HashMap::from([
                ("workspaceId".to_string(), "workspace-stats".to_string()),
                ("sessionId".to_string(), "session-stats".to_string()),
            ])),
        )
        .await
        .0;

        assert_eq!(payload["scope"], serde_json::json!("session"));
        assert_eq!(payload["workspaceId"], serde_json::json!("workspace-stats"));
        assert_eq!(payload["sessionId"], serde_json::json!("session-stats"));
        assert!(payload["items"].is_array());
        assert!(payload["totals"].is_object());
        assert!(payload["updatedAt"].is_number());
        assert!(payload["version"].is_number());
        assert!(payload.get("stats").is_none());
    }

    #[tokio::test]
    async fn session_stats_uses_model_usage_ledger_as_authority() {
        let state = test_state();
        let usage_payload = serde_json::json!({
            "workspaceId": "workspace-stats",
            "sessionId": "session-stats",
            "turnId": "turn-1",
            "eventId": "usage-model-1",
            "timestamp": 101,
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
                "urlMode": "default"
            },
            "callIdentity": {
                "callId": "call-1",
                "source": "orchestrator",
                "phase": "planning"
            },
            "usage": {
                "inputTokens": 12,
                "outputTokens": 5,
                "cacheReadTokens": 4,
                "cacheWriteTokens": 3
            },
            "status": "success"
        });
        state
            .event_bus
            .publish(
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
            )
            .expect("publish model usage event");

        let payload = session_stats(
            State(state.clone()),
            Query(HashMap::from([
                ("workspaceId".to_string(), "workspace-stats".to_string()),
                ("sessionId".to_string(), "session-stats".to_string()),
            ])),
        )
        .await
        .0;

        assert_eq!(payload["totals"]["netInputTokens"], serde_json::json!(12));
        assert_eq!(payload["totals"]["netOutputTokens"], serde_json::json!(5));
        assert_eq!(payload["totals"]["totalTokens"], serde_json::json!(17));
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

        let _ = reset_stats(
            State(state.clone()),
            Json(serde_json::json!({
                "workspaceId": "workspace-stats",
                "sessionId": "session-stats"
            })),
        )
        .await
        .expect("reset stats");
        let payload = session_stats(
            State(state),
            Query(HashMap::from([
                ("workspaceId".to_string(), "workspace-stats".to_string()),
                ("sessionId".to_string(), "session-stats".to_string()),
            ])),
        )
        .await
        .0;
        assert_eq!(payload["totals"]["totalTokens"], serde_json::json!(0));
        assert_eq!(payload["items"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn global_rules_and_safeguard_are_exposed_in_bootstrap() {
        let state = test_state();
        let session_a = SessionId::new("session-a");
        let session_b = SessionId::new("session-b");

        let user_rules_payload = serde_json::json!({
            "userRules": "【全局生效】"
        });
        let safeguard_payload = serde_json::json!({
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
        let saved_section = state.settings_store.get_section("safeguardConfig");
        let saved_rules = saved_section["rules"]
            .as_array()
            .expect("saved safeguard rules should be array");
        assert_eq!(saved_rules.len(), 3);
        assert!(
            saved_rules.iter().any(|rule| rule["pattern"]
                == serde_json::json!("custom-audit-global")
                && rule["action"] == serde_json::json!("audit_only")),
            "save path should trim pattern and preserve audit action"
        );
        assert!(
            saved_rules.iter().any(|rule| rule["pattern"]
                == serde_json::json!("ignored-empty-rule")
                && rule["category"] == serde_json::json!("custom")
                && rule["action"] == serde_json::json!("require_approval_in_restricted")),
            "save path should normalize unknown category/action exactly as SafetyGate does"
        );

        let bootstrap_a = settings_bootstrap(
            State(state.clone()),
            Query(HashMap::from([(
                "sessionId".to_string(),
                session_a.as_str().to_string(),
            )])),
        )
        .await
        .0;
        let bootstrap_b = settings_bootstrap(
            State(state.clone()),
            Query(HashMap::from([(
                "sessionId".to_string(),
                session_b.as_str().to_string(),
            )])),
        )
        .await
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
}
