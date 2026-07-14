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
use magi_core::{AccessProfile, SessionId, UtcMillis};
use magi_usage_authority::{
    SessionSummary, UsageAuthority, UsageCallRecordInput, UsageModelSnapshot, UsageTotals,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use super::session_scope;
use crate::{
    errors::{ApiError, settings_persistence_error},
    model_config::{
        NormalizedModelConfig, merge_orchestrator_session_override,
        reject_deprecated_model_config_fields, strip_orchestrator_session_owned_fields,
    },
    scope_binding::without_scope_binding_fields,
    state::ApiState,
};

fn unwrap_settings_section_request(
    request: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    if request.get("data").is_some() {
        return Err(ApiError::InvalidInput(
            "data 设置包装已废弃，请使用 config 或直接提交设置对象".to_string(),
        ));
    }
    Ok(request
        .get("config")
        .cloned()
        .unwrap_or_else(|| request.clone()))
}

fn scoped_settings_section_request(
    request: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    Ok(without_scope_binding_fields(
        unwrap_settings_section_request(request)?,
    ))
}

fn model_settings_section_request(
    request: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    let config = scoped_settings_section_request(request)?;
    reject_deprecated_model_config_fields(&config).map_err(ApiError::InvalidInput)?;
    Ok(config)
}

fn orchestrator_connection_section_request(
    request: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    let mut config = model_settings_section_request(request)?;
    if let Some(map) = config.as_object_mut() {
        map.remove("model");
        map.remove("reasoningEffort");
    }
    Ok(config)
}

pub(super) fn orchestrator_session_override_request(
    request: &serde_json::Value,
) -> Result<Value, ApiError> {
    let config = unwrap_settings_section_request(request)?;
    reject_deprecated_model_config_fields(&config).map_err(ApiError::InvalidInput)?;
    let Some(config) = config.as_object() else {
        return Err(ApiError::InvalidInput(
            "会话主模型配置必须是对象".to_string(),
        ));
    };

    let mut override_config = Map::new();
    if let Some(model) = config.get("model").and_then(Value::as_str).map(str::trim)
        && !model.is_empty()
    {
        override_config.insert("model".to_string(), Value::String(model.to_string()));
    }
    if config.contains_key("reasoningEffort") {
        match config.get("reasoningEffort") {
            Some(Value::Null) => {
                override_config.insert("reasoningEffort".to_string(), Value::Null);
            }
            Some(Value::String(value)) => {
                let value = value.trim();
                if !matches!(value, "low" | "medium" | "high" | "xhigh") {
                    return Err(ApiError::InvalidInput("reasoningEffort 无效".to_string()));
                }
                override_config.insert(
                    "reasoningEffort".to_string(),
                    Value::String(value.to_string()),
                );
            }
            Some(_) => {
                return Err(ApiError::InvalidInput(
                    "reasoningEffort 必须是字符串".to_string(),
                ));
            }
            None => {}
        }
    }
    Ok(Value::Object(override_config))
}

pub(super) fn save_orchestrator_session_override_for_session(
    state: &ApiState,
    session_id: &SessionId,
    config: &Value,
) -> Result<Option<Value>, ApiError> {
    let request = json!({ "config": config });
    let override_config = orchestrator_session_override_request(&request)?;
    let Some(override_fields) = override_config.as_object() else {
        return Ok(None);
    };
    if override_fields.is_empty() {
        return Ok(None);
    }

    let mut next_config = state
        .settings_store
        .get_session_section(session_id, "orchestrator");
    if !next_config.is_object() {
        next_config = json!({});
    }
    let next_fields = next_config
        .as_object_mut()
        .expect("session orchestrator config should be object");
    for (key, value) in override_fields {
        next_fields.insert(key.clone(), value.clone());
    }
    state
        .settings_store
        .set_session_section(session_id, "orchestrator", next_config.clone())
        .map_err(settings_persistence_error)?;
    Ok(Some(next_config))
}

fn parse_optional_query_string(query: &HashMap<String, String>, key: &str) -> Option<String> {
    query
        .get(key)
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn reject_deprecated_scope_query_fields(query: &HashMap<String, String>) -> Result<(), ApiError> {
    for key in [
        "session_id",
        "workspace_id",
        "workspace_path",
        "access_profile",
    ] {
        if query.contains_key(key) {
            return Err(ApiError::InvalidInput(format!(
                "{key} 已废弃，请使用 camelCase 查询字段"
            )));
        }
    }
    Ok(())
}

fn reject_deprecated_scope_body_fields(request: &Value) -> Result<(), ApiError> {
    for key in ["session_id", "workspace_id", "workspace_path"] {
        if request.get(key).is_some() {
            return Err(ApiError::InvalidInput(format!(
                "{key} 已废弃，请使用 camelCase scope 字段"
            )));
        }
    }
    Ok(())
}

fn parse_access_profile_query(query: &HashMap<String, String>) -> AccessProfile {
    query
        .get("accessProfile")
        .map(String::as_str)
        .and_then(|value| AccessProfile::from_str(value).ok())
        .unwrap_or_default()
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
    let config = NormalizedModelConfig::from_settings_value(&request.config)
        .map_err(ApiError::InvalidInput)?;
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
    let config = unwrap_settings_section_request(&request)?;
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

async fn fetch_model_ids_for_config(
    config: &NormalizedModelConfig,
) -> Result<Vec<String>, ApiError> {
    let url = config.models_list_url().map_err(ApiError::InvalidInput)?;
    let api_key = config.require_api_key().map_err(ApiError::InvalidInput)?;
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    match config.inferred_protocol() {
        HttpModelBridgeProtocol::ChatCompletions => {
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

    let response = reqwest::Client::new()
        .get(url)
        .headers(headers)
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
    Ok(parse_model_ids(&payload))
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

async fn probe_connection_response(request: Value) -> Result<Json<Value>, ApiError> {
    let config = parse_connection_probe_config(request)?;
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
            "/settings/orchestrator/test",
            post(test_orchestrator_connection),
        )
        .route("/settings/auxiliary/save", post(save_auxiliary_config))
        .route("/settings/auxiliary/test", post(test_auxiliary_connection))
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
            "description": "负责从根因落地边界清晰的实现，并完成清理与验证",
            "i18n": {
                "displayNameKey": "roleTemplate.executor.displayName",
                "descriptionKey": "roleTemplate.executor.description",
            },
            "defaultUI": { "colorToken": "agent-executor", "icon": "tool" },
            "profile": {
                "role": "executor",
                "focus": ["implementation", "integration", "cleanup", "verification"],
                "constraints": ["fix-at-source", "preserve-authoritative-state"],
                "outputPreferences": ["changes", "validation", "remaining-risk"],
            },
            "ownerships": ["implementation"],
            "insightPreferences": ["decision", "contract", "risk"],
        }),
        json!({
            "templateId": "explorer",
            "displayName": "Explorer",
            "description": "负责只读搜索、复现、证据收集与根因定位",
            "i18n": {
                "displayNameKey": "roleTemplate.explorer.displayName",
                "descriptionKey": "roleTemplate.explorer.description",
            },
            "defaultUI": { "colorToken": "agent-explorer", "icon": "bug" },
            "profile": {
                "role": "explorer",
                "focus": ["reproduction", "root-cause", "evidence", "data-flow"],
                "constraints": ["read-only", "no-assumption-without-evidence"],
                "outputPreferences": ["scope", "evidence", "next-step"],
            },
            "ownerships": ["investigation"],
            "insightPreferences": ["decision", "risk", "constraint"],
        }),
        json!({
            "templateId": "reviewer",
            "displayName": "Reviewer",
            "description": "负责独立审查行为回归、状态冲突与交付风险",
            "i18n": {
                "displayNameKey": "roleTemplate.reviewer.displayName",
                "descriptionKey": "roleTemplate.reviewer.description",
            },
            "defaultUI": { "colorToken": "agent-reviewer", "icon": "shield" },
            "profile": {
                "role": "reviewer",
                "focus": ["regression", "state-consistency", "security", "maintainability"],
                "constraints": ["read-only", "evidence-before-finding"],
                "outputPreferences": ["findings", "severity", "test-gaps"],
            },
            "ownerships": ["quality"],
            "insightPreferences": ["risk", "constraint", "decision"],
        }),
        json!({
            "templateId": "tester",
            "displayName": "Tester",
            "description": "负责测试矩阵、故障注入、真实场景与恢复验证",
            "i18n": {
                "displayNameKey": "roleTemplate.tester.displayName",
                "descriptionKey": "roleTemplate.tester.description",
            },
            "defaultUI": { "colorToken": "agent-tester", "icon": "check-circle" },
            "profile": {
                "role": "tester",
                "focus": ["test-matrix", "fault-injection", "recovery", "real-workflow"],
                "constraints": ["evidence-before-pass", "report-uncovered-scope"],
                "outputPreferences": ["matrix", "results", "uncovered-scope"],
            },
            "ownerships": ["verification"],
            "insightPreferences": ["risk", "constraint"],
        }),
        json!({
            "templateId": "architect",
            "displayName": "Architect",
            "description": "负责产品目标、用户工作流、系统边界与长期演进裁决",
            "i18n": {
                "displayNameKey": "roleTemplate.architect.displayName",
                "descriptionKey": "roleTemplate.architect.description",
            },
            "defaultUI": { "colorToken": "agent-architect", "icon": "grid" },
            "profile": {
                "role": "architect",
                "focus": ["product-intent", "user-workflow", "architecture", "boundaries"],
                "constraints": ["single-source-of-truth", "design-for-evolution"],
                "outputPreferences": ["decision", "data-flow", "acceptance-criteria"],
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
    let engine_id = entry
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let mut normalized = Map::new();
    normalized.insert("id".to_string(), Value::String(engine_id.to_string()));
    normalized.insert(
        "displayName".to_string(),
        Value::String(
            entry
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(engine_id)
                .to_string(),
        ),
    );
    normalized.insert(
        "llm".to_string(),
        entry
            .get("llm")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new())),
    );
    if let Some(runtime) = entry.get("runtime").cloned() {
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
    normalized
}

fn normalize_agent_override_entry(
    entry: &Value,
    template_ids: &HashSet<String>,
    order_map: &HashMap<String, usize>,
) -> Option<Value> {
    let template_id = entry
        .get("templateId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if !template_ids.contains(template_id) {
        return None;
    }
    // `engineId` 是「继承 vs 显式」的唯一字段：空串 = 继承编排模型，非空 = 显式绑定到 engine。
    let engine_id = entry
        .get("engineId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let order = order_map.get(template_id).copied().unwrap_or(0);
    let binding_revision = entry
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

    if let Some(ui_overrides) = entry.get("uiOverrides").cloned() {
        normalized.insert("uiOverrides".to_string(), ui_overrides);
    }
    if let Some(profile_overrides) = entry.get("profileOverrides").cloned() {
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
) -> Result<Json<serde_json::Value>, ApiError> {
    reject_deprecated_scope_query_fields(&query)?;
    let hydrate_mcp_servers = query
        .get("scope")
        .map(|value| value.trim())
        .is_none_or(|scope| scope != "core");
    let session_id = parse_optional_query_string(&query, "sessionId");
    let workspace_id = parse_optional_query_string(&query, "workspaceId");
    let workspace_path = parse_optional_query_string(&query, "workspacePath");
    let scope = session_scope::resolve_optional_session_workspace_scope(
        &state,
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
        if let Some(session_id) = scope.session_id() {
            let mut effective_orchestrator_config = object
                .get("orchestratorConfig")
                .cloned()
                .unwrap_or(Value::Null);
            strip_orchestrator_session_owned_fields(&mut effective_orchestrator_config);
            let session_orchestrator_config = state
                .settings_store
                .get_session_section(session_id, "orchestrator");
            merge_orchestrator_session_override(
                &mut effective_orchestrator_config,
                &session_orchestrator_config,
            );
            object.insert(
                "orchestratorSessionConfig".to_string(),
                session_orchestrator_config,
            );
            object.insert(
                "effectiveOrchestratorConfig".to_string(),
                effective_orchestrator_config,
            );
        } else {
            object.insert("orchestratorSessionConfig".to_string(), json!({}));
            object.insert(
                "effectiveOrchestratorConfig".to_string(),
                object
                    .get("orchestratorConfig")
                    .cloned()
                    .map(|mut value| {
                        strip_orchestrator_session_owned_fields(&mut value);
                        value
                    })
                    .unwrap_or_else(|| json!({})),
            );
        }
        object.insert(
            "workspaceId".to_string(),
            Value::String(scope.workspace_id_string()),
        );
        object.insert(
            "workspacePath".to_string(),
            Value::String(scope.workspace_path_string()),
        );
        object.insert(
            "sessionId".to_string(),
            Value::String(
                scope
                    .session_id()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            ),
        );
    }
    Ok(Json(snapshot))
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
        .set(&request.key, request.value.clone())
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

    if let (Some(worker_id), Some(worker_config)) = (worker_id, worker_config) {
        let worker_config = without_scope_binding_fields(worker_config.clone());
        reject_deprecated_model_config_fields(&worker_config).map_err(ApiError::InvalidInput)?;
        let mut workers = state
            .settings_store
            .get_section("workers")
            .as_object()
            .cloned()
            .unwrap_or_default();
        workers.insert(worker_id.to_string(), worker_config);
        state
            .settings_store
            .set_section("workers", serde_json::Value::Object(workers))
            .map_err(settings_persistence_error)?;
    } else {
        state
            .settings_store
            .set_section("workers", model_settings_section_request(&request)?)
            .map_err(settings_persistence_error)?;
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
    state
        .settings_store
        .remove_section_entry("workers", worker)
        .map_err(settings_persistence_error)?;
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
        .set_section(
            "orchestrator",
            orchestrator_connection_section_request(&request)?,
        )
        .map_err(settings_persistence_error)?;
    Ok(Json(serde_json::json!({ "saved": true })))
}

async fn save_orchestrator_session_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    reject_deprecated_scope_body_fields(&request)?;
    let session_id = request.get("sessionId").and_then(Value::as_str);
    let workspace_id = request.get("workspaceId").and_then(Value::as_str);
    let workspace_path = request.get("workspacePath").and_then(Value::as_str);
    let scope = session_scope::require_session_workspace_scope(
        &state,
        session_id,
        workspace_id,
        workspace_path,
        "保存会话主模型配置",
    )?;
    let override_config = orchestrator_session_override_request(&request)?;
    if override_config
        .as_object()
        .is_none_or(|config| config.is_empty())
    {
        state
            .settings_store
            .remove_session_section(&scope.session_id, "orchestrator")
            .map_err(settings_persistence_error)?;
    } else {
        state
            .settings_store
            .set_session_section(&scope.session_id, "orchestrator", override_config.clone())
            .map_err(settings_persistence_error)?;
    }

    let mut effective_config = state.settings_store.get_section("orchestrator");
    strip_orchestrator_session_owned_fields(&mut effective_config);
    merge_orchestrator_session_override(&mut effective_config, &override_config);
    Ok(Json(serde_json::json!({
        "saved": true,
        "sessionId": scope.session_id.to_string(),
        "workspaceId": scope.workspace_id.to_string(),
        "orchestratorSessionConfig": override_config,
        "effectiveOrchestratorConfig": effective_config,
    })))
}

async fn test_orchestrator_connection(
    State(_state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let config = unwrap_settings_section_request(&request)?;
    let normalized =
        NormalizedModelConfig::from_settings_value(&config).map_err(ApiError::InvalidInput)?;
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
    state
        .settings_store
        .set_section("auxiliary", model_settings_section_request(&request)?)
        .map_err(settings_persistence_error)?;
    Ok(Json(serde_json::json!({ "saved": true })))
}

async fn test_auxiliary_connection(
    State(_state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    probe_connection_response(request).await
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
    for field in ["baseUrl", "apiKey", "model", "urlMode"] {
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
    state
        .settings_store
        .set_section(
            "imageGeneration",
            image_generation_section_request(&request)?,
        )
        .map_err(settings_persistence_error)?;
    Ok(Json(json!({ "saved": true })))
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

async fn save_safeguard_config(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let normalized =
        crate::state::normalize_safeguard_config_value(scoped_settings_section_request(&request)?);
    state
        .settings_store
        .set_section("safeguardConfig", normalized)
        .map_err(settings_persistence_error)?;
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
    if request.get("engine").is_some() || request.get("engineId").is_some() {
        return Err(ApiError::InvalidInput(
            "引擎配置必须使用顶层 id/displayName/llm 字段".to_string(),
        ));
    }
    if let Some(llm) = request.get("llm") {
        reject_deprecated_model_config_fields(llm).map_err(ApiError::InvalidInput)?;
    }
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
        .set_section("engines", Value::Array(engines.clone()))
        .map_err(settings_persistence_error)?;
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
    let mut engines = load_registry_engines(&state);
    engines.retain(|entry| {
        entry
            .get("id")
            .and_then(Value::as_str)
            .is_none_or(|value| value != engine_id)
    });
    state
        .settings_store
        .set_section("engines", Value::Array(engines.clone()))
        .map_err(settings_persistence_error)?;
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
    if request.get("agent").is_some() || request.get("modelSource").is_some() {
        return Err(ApiError::InvalidInput(
            "角色绑定必须使用顶层 templateId/engineId 字段".to_string(),
        ));
    }
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
            return Err(ApiError::conflict("角色绑定的引擎不存在", engine_id));
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
        .set_section("agents", Value::Array(overrides))
        .map_err(settings_persistence_error)?;

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
        .set_section("agents", Value::Array(overrides))
        .map_err(settings_persistence_error)?;
    Ok(Json(json!({ "agents": resolve_registry_agents(&state) })))
}

async fn fetch_models(
    State(_state): State<ApiState>,
    Json(request): Json<FetchModelsRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (config, target) = parse_fetch_models_config(request)?;
    let now = UtcMillis::now();
    let models = fetch_model_ids_for_config(&config).await?;
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
) -> Result<Json<serde_json::Value>, ApiError> {
    reject_deprecated_scope_query_fields(&query)?;
    let workspace_id = session_scope::require_registered_workspace_binding(
        &state,
        parse_optional_query_string(&query, "workspaceId").as_deref(),
        parse_optional_query_string(&query, "workspacePath").as_deref(),
    )?
    .workspace_id
    .to_string();
    let session_id = parse_optional_query_string(&query, "sessionId");
    let mut authority = usage_authority_from_model_usage_ledger(&state);
    if let Some(session_id) = session_id.as_deref() {
        let snapshot = authority.get_session_snapshot(&workspace_id, session_id);
        Ok(Json(serde_json::json!({
            "scope": "session",
            "workspaceId": snapshot.workspace_id,
            "sessionId": snapshot.session_id,
            "version": snapshot.version,
            "lastAppliedLedgerSeq": snapshot.last_applied_ledger_seq,
            "updatedAt": snapshot.updated_at,
            "totals": usage_totals_json(&snapshot.totals),
            "items": snapshot.by_execution_binding.into_iter().map(usage_binding_item_json).collect::<Vec<_>>(),
            "models": snapshot.by_model_identity.into_iter().map(usage_model_item_json).collect::<Vec<_>>(),
        })))
    } else {
        let snapshot = authority.get_workspace_snapshot(&workspace_id);
        Ok(Json(serde_json::json!({
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
        })))
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
    reject_deprecated_scope_body_fields(&request)?;
    let workspace_id_value = request
        .get("workspaceId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let workspace_path_value = request
        .get("workspacePath")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let workspace_id = session_scope::require_registered_workspace_binding(
        &state,
        workspace_id_value,
        workspace_path_value,
    )?
    .workspace_id
    .to_string();
    let session_id = request
        .get("sessionId")
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
            .is_some_and(|value| value == workspace_id);
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
    use axum::http::StatusCode;
    use magi_core::{AbsolutePath, EventId, SessionId, WorkspaceId};
    use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
    use magi_governance::GovernanceService;
    use magi_session_store::{SessionRecord, SessionStore};
    use magi_snapshot::SnapshotManager;
    use magi_tool_runtime::{
        ExternalMcpServerCatalogEntry, ExternalToolCatalogSnapshot, ToolRegistry,
    };
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;

    fn test_state() -> ApiState {
        test_state_with_external_catalog_snapshot(None)
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
            crate::state::build_runtime_capability_dependency_provider(
                snapshot_manager.clone(),
                workspace_store.clone(),
                true,
            );
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus))
            .with_runtime_capability_dependency_provider(runtime_capability_dependency_provider);
        if let Some(snapshot) = external_catalog_snapshot {
            tool_registry = tool_registry
                .with_external_tool_catalog_provider(Arc::new(move || snapshot.clone()));
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

    fn test_session_record(
        session_id: &SessionId,
        workspace_id: &str,
        title: &str,
    ) -> SessionRecord {
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
                "urlMode": "default"
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
        assert_eq!(payload["messages"][0]["content"], json!("ping"));
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
            "urlMode": "standard"
        }))
        .expect("模型配置应符合当前协议");
        execute_connection_probe(&config)
            .await
            .expect("anthropic probe should succeed");
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

        let result = probe_connection_response(json!({
            "baseUrl": base_url,
            "apiKey": "test-key",
            "model": "gpt-test",
            "urlMode": "standard"
        }))
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
                "config": {
                    "baseUrl": base_url,
                    "apiKey": "test-key",
                    "urlMode": "standard"
                }
            })),
        )
        .await;

        let _ =
            result.expect("orchestrator connection-only probe should pass through model catalog");
        server.abort();
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
                "config": {
                    "baseUrl": base_url,
                    "apiKey": "test-key",
                    "model": "gpt-test",
                    "urlMode": "standard"
                }
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
                "config": {
                    "baseUrl": base_url,
                    "apiKey": "test-image-key",
                    "model": "gpt-image-test",
                    "urlMode": "standard"
                }
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
                    "urlMode": "standard"
                }),
                target: "orchestrator".to_string(),
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

        let bootstrap = settings_bootstrap(
            State(state),
            Query(HashMap::from([
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
        assert_eq!(bootstrap["workspacePath"], json!(workspace_path));
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
        assert_eq!(builtin_tools.len(), 28);
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
                "image_generate",
                "knowledge_query",
                "code_symbols",
                "tool_catalog",
                "get_goal",
                "create_goal",
                "update_goal",
                "agent_spawn",
                "agent_wait",
                "todo_write",
                "memory_write",
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
        assert_eq!(
            capability_dependencies[6]["sessionId"],
            serde_json::json!("session-empty-contract")
        );
        assert_eq!(
            capability_dependencies[7]["name"],
            serde_json::json!("file_snapshot")
        );
        assert_eq!(
            capability_dependencies[7]["status"],
            serde_json::json!("ready"),
            "snapshot dependency should report the lazy snapshot capability as ready before the session starts"
        );
        assert_eq!(
            capability_dependencies[7]["sessionId"],
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
        assert_eq!(bootstrap["workspacePath"], json!(workspace_path));
    }

    #[tokio::test]
    async fn settings_scope_rejects_deprecated_snake_case_fields() {
        let bootstrap_result = settings_bootstrap(
            State(test_state()),
            Query(HashMap::from([(
                "session_id".to_string(),
                "session-old".to_string(),
            )])),
        )
        .await;
        assert_deprecated_scope_field_error(bootstrap_result, "session_id");

        let access_profile_result = settings_bootstrap(
            State(test_state()),
            Query(HashMap::from([(
                "access_profile".to_string(),
                "full_access".to_string(),
            )])),
        )
        .await;
        assert_deprecated_scope_field_error(access_profile_result, "access_profile");

        let stats_result = session_stats(
            State(test_state()),
            Query(HashMap::from([(
                "workspace_id".to_string(),
                "workspace-old".to_string(),
            )])),
        )
        .await;
        assert_deprecated_scope_field_error(stats_result, "workspace_id");

        let reset_result = reset_stats(
            State(test_state()),
            Json(json!({ "workspace_path": "/tmp/old" })),
        )
        .await;
        assert_deprecated_scope_field_error(reset_result, "workspace_path");

        let session_save_result = save_orchestrator_session_config(
            State(test_state()),
            Json(json!({ "session_id": "session-old" })),
        )
        .await;
        assert_deprecated_scope_field_error(session_save_result, "session_id");
    }

    fn assert_deprecated_scope_field_error<T: std::fmt::Debug>(
        result: Result<Json<T>, ApiError>,
        expected_field: &str,
    ) {
        match result {
            Err(ApiError::InvalidInput(message)) => {
                assert!(message.contains(expected_field), "{message}");
                assert!(message.contains("已废弃"), "{message}");
            }
            other => panic!("expected deprecated scope field error, got {other:?}"),
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
            .create_session_for_workspace(
                session_b.clone(),
                "B 会话",
                Some(workspace_b.to_string()),
            )
            .expect("session should create");

        let result = settings_bootstrap(
            State(state),
            Query(HashMap::from([
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
    async fn settings_bootstrap_removes_deprecated_model_provider_fields() {
        let state = test_state();
        state
            .settings_store
            .set_section(
                "orchestrator",
                json!({
                    "provider": "anthropic",
                    "baseUrl": "https://api.anthropic.com",
                    "model": "claude-sonnet-test",
                    "urlMode": "standard"
                }),
            )
            .unwrap();
        state
            .settings_store
            .set_section(
                "workers",
                json!({
                    "sonnet-worker": {
                        "provider": "anthropic",
                        "baseUrl": "https://api.anthropic.com",
                        "model": "claude-worker-test",
                        "urlMode": "standard"
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
                        "provider": "anthropic",
                        "baseUrl": "https://api.anthropic.com",
                        "model": "claude-worker-test",
                        "urlMode": "standard"
                    }
                }]),
            )
            .unwrap();

        let bootstrap = settings_bootstrap(State(state), Query(HashMap::new()))
            .await
            .expect("settings bootstrap should build")
            .0;

        assert!(bootstrap["orchestratorConfig"].get("provider").is_none());
        assert!(
            bootstrap["workerConfigs"]["sonnet-worker"]
                .get("provider")
                .is_none()
        );
        assert!(
            bootstrap["registryEngines"][0]["llm"]
                .get("provider")
                .is_none()
        );
        assert_eq!(
            bootstrap["workerConfigs"]["sonnet-worker"]["model"],
            json!("claude-worker-test")
        );
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
                    "model": "current-main",
                    "urlMode": "standard"
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
                    "urlMode": "standard"
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

        let bootstrap = settings_bootstrap(State(state), Query(HashMap::new()))
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
                        "provider": "anthropic",
                        "baseUrl": "https://api.anthropic.com",
                        "model": "claude-worker-test",
                        "urlMode": "standard"
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
                        "provider": "openai",
                        "baseUrl": "https://api.openai.com",
                        "model": "stale-openai-model",
                        "urlMode": "standard"
                    }
                }]),
            )
            .unwrap();

        let bootstrap = settings_bootstrap(State(state.clone()), Query(HashMap::new()))
            .await
            .expect("settings bootstrap should build")
            .0;

        assert_eq!(
            bootstrap["registryEngines"][0]["llm"]["model"],
            json!("claude-worker-test")
        );
        assert!(
            bootstrap["registryEngines"][0]["llm"]
                .get("provider")
                .is_none()
        );
        let persisted_engines = state.settings_store.get_section("engines");
        assert!(persisted_engines[0]["llm"].get("provider").is_none());
        assert_eq!(
            persisted_engines[0]["llm"]["model"],
            json!("stale-openai-model")
        );
    }

    #[test]
    fn normalize_engine_entry_keeps_canonical_model_config() {
        let normalized = normalize_engine_entry(&json!({
            "id": "sonnet-4-5",
            "displayName": "sonnet-4.5",
            "llm": {
                "baseUrl": "http://localhost:8317/",
                "model": "kiro-claude-sonnet-4-5-agentic",
                "urlMode": "standard"
            }
        }))
        .expect("engine should normalize");

        assert_eq!(
            normalized["llm"]["baseUrl"],
            json!("http://localhost:8317/")
        );
        assert_eq!(
            normalized["llm"]["model"],
            json!("kiro-claude-sonnet-4-5-agentic")
        );
    }

    #[tokio::test]
    async fn registry_engine_upsert_rejects_legacy_engine_wrappers() {
        let state = test_state();
        let result = upsert_engine(
            State(state.clone()),
            Json(json!({
                "engine": {
                    "id": "legacy-wrapper",
                    "llm": {}
                }
            })),
        )
        .await;
        match result {
            Err(ApiError::InvalidInput(message)) => {
                assert!(message.contains("顶层 id/displayName/llm"));
            }
            other => panic!("expected invalid input, got {other:?}"),
        }

        let result = upsert_engine(
            State(state),
            Json(json!({
                "engineId": "legacy-engine-id",
                "llm": {}
            })),
        )
        .await;
        match result {
            Err(ApiError::InvalidInput(message)) => {
                assert!(message.contains("顶层 id/displayName/llm"));
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn registry_agent_upsert_rejects_legacy_wrapper_and_model_source() {
        let state = test_state();
        let result = upsert_agent(
            State(state.clone()),
            Json(json!({
                "agent": {
                    "templateId": "reviewer",
                    "engineId": ""
                }
            })),
        )
        .await;
        match result {
            Err(ApiError::InvalidInput(message)) => {
                assert!(message.contains("顶层 templateId/engineId"));
            }
            other => panic!("expected invalid input, got {other:?}"),
        }

        let result = upsert_agent(
            State(state),
            Json(json!({
                "templateId": "reviewer",
                "modelSource": "engine",
                "engineId": ""
            })),
        )
        .await;
        match result {
            Err(ApiError::InvalidInput(message)) => {
                assert!(message.contains("顶层 templateId/engineId"));
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
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
        )
        .unwrap();

        let bootstrap = settings_bootstrap(State(state), Query(HashMap::new()))
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
        assert!(servers.iter().any(|server| {
            server["id"] == json!("valid-server") && server["serverId"] == json!("valid-server")
        }));
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
    async fn settings_bootstrap_redacts_mcp_env_values() {
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

        let bootstrap = settings_bootstrap(State(state), Query(HashMap::new()))
            .await
            .expect("settings bootstrap should build")
            .0;

        assert_eq!(
            bootstrap["mcpServers"][0]["env"]["TOKEN"],
            json!(crate::mcp_config::REDACTED_MCP_ENV_VALUE)
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

        let bootstrap = settings_bootstrap(State(state), Query(HashMap::new()))
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

        let bootstrap = settings_bootstrap(State(state), Query(HashMap::new()))
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
            Query(HashMap::from([("scope".to_string(), "core".to_string())])),
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
            "config": {
                "provider": "openai",
                "workspaceId": "workspace-a",
                "workspace_id": "workspace-b",
                "workspacePath": "/tmp/a",
                "workspace_path": "/tmp/b",
                "sessionId": "session-a",
                "session_id": "session-b"
            }
        }))
        .expect("config wrapper should be accepted");

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

    #[test]
    fn scoped_settings_section_request_rejects_deprecated_data_wrapper() {
        let error = scoped_settings_section_request(&json!({
            "data": {
                "baseUrl": "https://api.example.com/v1"
            }
        }))
        .expect_err("data wrapper must not remain a settings input path");

        match error {
            ApiError::InvalidInput(message) => {
                assert!(message.contains("data"));
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn model_settings_section_request_rejects_deprecated_provider_field() {
        let error = model_settings_section_request(&json!({
            "config": {
                "provider": "openai",
                "baseUrl": "https://api.example.com/v1",
                "apiKey": "sk-test",
                "model": "gpt-test"
            }
        }))
        .expect_err("模型配置保存入口必须拒绝 provider 输入字段");

        match error {
            ApiError::InvalidInput(message) => {
                assert!(message.contains("provider"));
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn save_orchestrator_config_writes_only_global_connection() {
        let state = test_state();
        let _ = save_orchestrator_config(
            State(state.clone()),
            Json(json!({
                "config": {
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-global",
                    "model": "global-main-model",
                    "reasoningEffort": "high",
                    "sessionId": "session-a",
                    "workspaceId": "workspace-a"
                }
            })),
        )
        .await
        .expect("orchestrator config should save");

        let saved = state.settings_store.get_section("orchestrator");
        assert_eq!(saved["baseUrl"], json!("https://api.example.com/v1"));
        assert_eq!(saved["apiKey"], json!("sk-global"));
        assert!(saved.get("model").is_none());
        assert!(saved.get("reasoningEffort").is_none());
        assert!(saved.get("sessionId").is_none());
        assert!(saved.get("workspaceId").is_none());
        assert!(
            state
                .settings_store
                .get_session_section(&SessionId::new("session-a"), "orchestrator")
                .is_null(),
            "全局连接保存接口不得写会话模型覆盖"
        );
    }

    #[tokio::test]
    async fn save_image_generation_config_persists_canonical_model_fields() {
        let state = test_state();
        let _ = save_image_generation_config(
            State(state.clone()),
            Json(json!({
                "config": {
                    "baseUrl": "https://cpa.example.com/v1",
                    "apiKey": "sk-image",
                    "model": "gpt-image-1",
                    "urlMode": "standard",
                    "reasoningEffort": "high",
                    "workspaceId": "workspace-ignored"
                }
            })),
        )
        .await
        .expect("image generation config should save");

        let saved = state.settings_store.get_section("imageGeneration");
        assert_eq!(saved["baseUrl"], json!("https://cpa.example.com/v1"));
        assert_eq!(saved["apiKey"], json!("sk-image"));
        assert_eq!(saved["model"], json!("gpt-image-1"));
        assert_eq!(saved["urlMode"], json!("standard"));
        assert!(saved.get("reasoningEffort").is_none());
        assert!(saved.get("workspaceId").is_none());
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
        state
            .settings_store
            .set_section(
                "orchestrator",
                json!({
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-global",
                    "model": "global-main-model",
                    "reasoningEffort": "medium"
                }),
            )
            .unwrap();

        let response = save_orchestrator_session_config(
            State(state.clone()),
            Json(json!({
                "sessionId": session_id.as_str(),
                "workspaceId": "workspace-session-model",
                "config": {
                    "baseUrl": "https://malicious.example.com/v1",
                    "apiKey": "sk-session-should-drop",
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
        assert_eq!(global["baseUrl"], json!("https://api.example.com/v1"));
        assert_eq!(global["apiKey"], json!("sk-global"));
        let saved = state
            .settings_store
            .get_session_section(&session_id, "orchestrator");
        assert_eq!(saved["model"], json!("session-main-model"));
        assert_eq!(saved["reasoningEffort"], json!("high"));
        assert!(
            saved.get("baseUrl").is_none() && saved.get("apiKey").is_none(),
            "会话覆盖不得持久化连接凭据"
        );
        assert_eq!(
            response["effectiveOrchestratorConfig"]["baseUrl"],
            json!("https://api.example.com/v1")
        );
        assert_eq!(
            response["effectiveOrchestratorConfig"]["model"],
            json!("session-main-model")
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
        state
            .settings_store
            .set_section(
                "orchestrator",
                json!({
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-global",
                    "model": "global-main-model",
                    "reasoningEffort": "medium"
                }),
            )
            .unwrap();
        state
            .settings_store
            .set_session_section(
                &session_id,
                "orchestrator",
                json!({
                    "model": "session-main-model",
                    "reasoningEffort": "xhigh"
                }),
            )
            .unwrap();

        let bootstrap = settings_bootstrap(
            State(state.clone()),
            Query(HashMap::from([
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

    #[test]
    fn fetch_models_config_rejects_deprecated_model_provider_field() {
        let error = parse_fetch_models_config(FetchModelsRequest {
            config: serde_json::json!({
                "provider": "openai",
                "baseUrl": "http://127.0.0.1:8320/v1",
                "apiKey": "test-key",
                "urlMode": "standard"
            }),
            target: "orch".to_string(),
        })
        .expect_err("fetch models config should reject deprecated provider input");

        match error {
            ApiError::InvalidInput(message) => {
                assert!(message.contains("provider"));
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_stats_returns_frontend_stats_contract() {
        let state = test_state();
        let _workspace_root = register_test_workspace(&state, "workspace-stats");
        let payload = session_stats(
            State(state),
            Query(HashMap::from([
                ("workspaceId".to_string(), "workspace-stats".to_string()),
                ("sessionId".to_string(), "session-stats".to_string()),
            ])),
        )
        .await
        .expect("session stats should build")
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

        let payload = session_stats(
            State(state.clone()),
            Query(HashMap::from([
                ("workspaceId".to_string(), "workspace-stats".to_string()),
                ("sessionId".to_string(), "session-stats".to_string()),
            ])),
        )
        .await
        .expect("session stats should build")
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
        .expect("session stats should build after reset")
        .0;
        assert_eq!(payload["totals"]["totalTokens"], serde_json::json!(0));
        assert_eq!(payload["items"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn session_stats_requires_registered_workspace_scope() {
        let result = session_stats(
            State(test_state()),
            Query(HashMap::from([(
                "sessionId".to_string(),
                "session-stats".to_string(),
            )])),
        )
        .await;

        match result {
            Err(ApiError::InvalidInput(message)) => {
                assert_eq!(message, "workspaceId 不能为空");
            }
            other => panic!("unexpected session stats result: {:?}", other),
        }
    }

    #[tokio::test]
    async fn reset_stats_requires_registered_workspace_scope() {
        let result = reset_stats(
            State(test_state()),
            Json(serde_json::json!({
                "sessionId": "session-stats"
            })),
        )
        .await;

        match result {
            Err(ApiError::InvalidInput(message)) => {
                assert_eq!(message, "workspaceId 不能为空");
            }
            other => panic!("unexpected reset stats result: {:?}", other),
        }
    }

    #[tokio::test]
    async fn reset_stats_only_removes_usage_with_matching_workspace_context() {
        let state = test_state();
        let _workspace_root = register_test_workspace(&state, "workspace-stats");
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

        let before_reset = session_stats(
            State(state.clone()),
            Query(HashMap::from([
                ("workspaceId".to_string(), "workspace-stats".to_string()),
                ("sessionId".to_string(), "session-stats".to_string()),
            ])),
        )
        .await
        .expect("session stats should build before reset")
        .0;
        assert_eq!(before_reset["totals"]["totalTokens"], serde_json::json!(54));

        let _ = reset_stats(
            State(state.clone()),
            Json(serde_json::json!({
                "workspaceId": "workspace-stats",
                "sessionId": "session-stats"
            })),
        )
        .await
        .expect("reset stats should succeed");

        let after_reset = session_stats(
            State(state.clone()),
            Query(HashMap::from([
                ("workspaceId".to_string(), "workspace-stats".to_string()),
                ("sessionId".to_string(), "session-stats".to_string()),
            ])),
        )
        .await
        .expect("session stats should build after reset")
        .0;
        assert_eq!(after_reset["totals"]["totalTokens"], serde_json::json!(37));

        let ledger_snapshot = state.event_bus.audit_usage_ledger_snapshot();
        assert_eq!(ledger_snapshot.usage_entries.len(), 1);
        assert_eq!(ledger_snapshot.usage_entries[0].event_id, "usage-unscoped");
        assert!(
            ledger_snapshot.usage_entries[0]
                .context
                .workspace_id
                .is_none(),
            "reset must not treat missing event context as matching the current workspace"
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
        .expect("settings bootstrap A should build")
        .0;
        let bootstrap_b = settings_bootstrap(
            State(state.clone()),
            Query(HashMap::from([(
                "sessionId".to_string(),
                session_b.as_str().to_string(),
            )])),
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
}
