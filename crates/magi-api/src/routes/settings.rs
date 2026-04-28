use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_core::UtcMillis;
use magi_usage_authority::{
    SessionSummary, UsageAuthority, UsageCallRecordInput, UsageModelSnapshot, UsageTotals,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

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
        for key in ["workspaceId", "workspace_id", "sessionId", "session_id"] {
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
    config.require_base_url()?;
    config.require_api_key()?;
    config.require_openai_models_listable()?;
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
    let normalized = NormalizedModelConfig::from_settings_value(&config, "anthropic");
    normalized.require_base_url()?;
    normalized.require_api_key()?;
    normalized.require_model()?;
    Ok(normalized)
}

async fn execute_connection_probe(
    config: &NormalizedModelConfig,
) -> Result<(u16, Value), ApiError> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let provider_key = config.provider_key();
    let (url, body) = match provider_key.as_str() {
        "openai" => {
            let auth_value =
                HeaderValue::from_str(&format!("Bearer {}", config.require_api_key()?))
                    .map_err(|_| ApiError::InvalidInput("apiKey 包含非法字符".to_string()))?;
            headers.insert(AUTHORIZATION, auth_value);
            (config.openai_probe_url()?, config.openai_probe_body()?)
        }
        "anthropic" => {
            let api_key_name = HeaderName::from_static("x-api-key");
            let api_key_value = HeaderValue::from_str(config.require_api_key()?)
                .map_err(|_| ApiError::InvalidInput("apiKey 包含非法字符".to_string()))?;
            headers.insert(api_key_name, api_key_value);
            headers.insert(
                HeaderName::from_static("anthropic-version"),
                HeaderValue::from_static("2023-06-01"),
            );
            (
                config.anthropic_probe_url()?,
                config.anthropic_probe_body()?,
            )
        }
        _ => {
            return Err(ApiError::InvalidInput(format!(
                "暂不支持 {} 提供方的真实连接测试",
                config.provider()
            )));
        }
    };

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
    vec![
        json!({
            "templateId": "frontend-dev",
            "displayName": "Frontend Engineer",
            "description": "负责界面实现、交互体验与前端状态流",
            "i18n": {
                "displayNameKey": "roleTemplate.frontend-dev.displayName",
                "descriptionKey": "roleTemplate.frontend-dev.description",
            },
            "defaultUI": { "colorToken": "agent-frontend-dev", "icon": "code" },
            "profile": {
                "role": "frontend-dev",
                "focus": ["ui", "interaction", "state", "rendering"],
                "constraints": ["preserve-design-system", "keep-runtime-contracts"],
                "outputPreferences": ["diff", "ux-impact", "follow-up"],
            },
            "ownerships": ["web", "frontend-contracts"],
            "insightPreferences": ["contract", "risk", "constraint"],
        }),
        json!({
            "templateId": "backend-dev",
            "displayName": "Backend Engineer",
            "description": "负责服务接口、状态流转与运行时主链实现",
            "i18n": {
                "displayNameKey": "roleTemplate.backend-dev.displayName",
                "descriptionKey": "roleTemplate.backend-dev.description",
            },
            "defaultUI": { "colorToken": "agent-backend-dev", "icon": "tool" },
            "profile": {
                "role": "backend-dev",
                "focus": ["api", "runtime", "storage", "contracts"],
                "constraints": ["preserve-authoritative-state", "avoid-duplication"],
                "outputPreferences": ["diff", "runtime-impact", "follow-up"],
            },
            "ownerships": ["crates", "apps"],
            "insightPreferences": ["decision", "contract", "risk"],
        }),
        json!({
            "templateId": "reviewer",
            "displayName": "Code Review Engineer",
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
            "templateId": "test-engineer",
            "displayName": "Test Engineer",
            "description": "负责验证链路、场景覆盖与失败复现",
            "i18n": {
                "displayNameKey": "roleTemplate.test-engineer.displayName",
                "descriptionKey": "roleTemplate.test-engineer.description",
            },
            "defaultUI": { "colorToken": "agent-test-engineer", "icon": "check-circle" },
            "profile": {
                "role": "test-engineer",
                "focus": ["verification", "coverage", "smoke", "repro"],
                "constraints": ["prefer-real-paths", "keep-signal-high"],
                "outputPreferences": ["steps", "result", "follow-up"],
            },
            "ownerships": ["verification"],
            "insightPreferences": ["risk", "constraint"],
        }),
        json!({
            "templateId": "doc-writer",
            "displayName": "Documentation Engineer",
            "description": "负责沉淀接口说明、迁移结论与交付说明",
            "i18n": {
                "displayNameKey": "roleTemplate.doc-writer.displayName",
                "descriptionKey": "roleTemplate.doc-writer.description",
            },
            "defaultUI": { "colorToken": "agent-doc-writer", "icon": "document" },
            "profile": {
                "role": "doc-writer",
                "focus": ["docs", "handoff", "clarity"],
                "constraints": ["reflect-runtime-truth", "avoid-stale-docs"],
                "outputPreferences": ["summary", "decision", "follow-up"],
            },
            "ownerships": ["docs"],
            "insightPreferences": ["contract", "constraint"],
        }),
        json!({
            "templateId": "debugger",
            "displayName": "Debugging Engineer",
            "description": "负责问题定位、根因分析与修复闭环",
            "i18n": {
                "displayNameKey": "roleTemplate.debugger.displayName",
                "descriptionKey": "roleTemplate.debugger.description",
            },
            "defaultUI": { "colorToken": "agent-debugger", "icon": "bug" },
            "profile": {
                "role": "debugger",
                "focus": ["root-cause", "runtime", "state-drift", "repair"],
                "constraints": ["fix-at-source", "no-patchy-workarounds"],
                "outputPreferences": ["root-cause", "fix", "verification"],
            },
            "ownerships": ["incident-response"],
            "insightPreferences": ["decision", "risk", "constraint"],
        }),
        json!({
            "templateId": "integration-dev",
            "displayName": "Integration Engineer",
            "description": "负责跨模块、跨端与跨服务联调收口",
            "i18n": {
                "displayNameKey": "roleTemplate.integration-dev.displayName",
                "descriptionKey": "roleTemplate.integration-dev.description",
            },
            "defaultUI": { "colorToken": "agent-integration-dev", "icon": "tools" },
            "profile": {
                "role": "integration-dev",
                "focus": ["integration", "contracts", "read-model", "events"],
                "constraints": ["single-source-of-truth", "preserve-e2e-flow"],
                "outputPreferences": ["diff", "integration-impact", "follow-up"],
            },
            "ownerships": ["integration"],
            "insightPreferences": ["contract", "risk", "decision"],
        }),
        json!({
            "templateId": "data-engineer",
            "displayName": "Data Engineer",
            "description": "负责数据投影、索引、提取与数据契约",
            "i18n": {
                "displayNameKey": "roleTemplate.data-engineer.displayName",
                "descriptionKey": "roleTemplate.data-engineer.description",
            },
            "defaultUI": { "colorToken": "agent-data-engineer", "icon": "stats" },
            "profile": {
                "role": "data-engineer",
                "focus": ["data-model", "projection", "indexing", "lineage"],
                "constraints": ["preserve-data-shape", "avoid-silent-loss"],
                "outputPreferences": ["diff", "data-impact", "follow-up"],
            },
            "ownerships": ["data"],
            "insightPreferences": ["contract", "risk"],
        }),
        json!({
            "templateId": "devops-engineer",
            "displayName": "DevOps Engineer",
            "description": "负责环境、部署、运行配置与运维可用性",
            "i18n": {
                "displayNameKey": "roleTemplate.devops-engineer.displayName",
                "descriptionKey": "roleTemplate.devops-engineer.description",
            },
            "defaultUI": { "colorToken": "agent-devops-engineer", "icon": "tools" },
            "profile": {
                "role": "devops-engineer",
                "focus": ["environment", "operations", "deployment", "health"],
                "constraints": ["preserve-operability", "prefer-observable-changes"],
                "outputPreferences": ["diff", "operational-impact", "follow-up"],
            },
            "ownerships": ["operations"],
            "insightPreferences": ["risk", "constraint"],
        }),
        json!({
            "templateId": "security-analyst",
            "displayName": "Security Engineer",
            "description": "负责权限、数据暴露面与安全风险评估",
            "i18n": {
                "displayNameKey": "roleTemplate.security-analyst.displayName",
                "descriptionKey": "roleTemplate.security-analyst.description",
            },
            "defaultUI": { "colorToken": "agent-security-analyst", "icon": "shield" },
            "profile": {
                "role": "security-analyst",
                "focus": ["security", "auth", "exposure", "governance"],
                "constraints": ["least-privilege", "surface-security-risk"],
                "outputPreferences": ["findings", "severity", "follow-up"],
            },
            "ownerships": ["security"],
            "insightPreferences": ["risk", "constraint", "decision"],
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
        "modelSource": "orchestrator",
        "engineId": "",
        "enabled": true,
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
        raw.get("llm").cloned().unwrap_or_else(|| json!({})),
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

pub(crate) fn load_registry_engines(state: &ApiState) -> Vec<Value> {
    let raw_engines = state.settings_store.get_section("engines");
    let normalized = normalize_engine_entries(&raw_engines);
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
    let raw_engine_id = raw
        .get("engineId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let model_source = match raw.get("modelSource").and_then(Value::as_str) {
        Some("engine") if !raw_engine_id.is_empty() => "engine",
        Some("orchestrator") => "orchestrator",
        _ if !raw_engine_id.is_empty() => "engine",
        _ => "orchestrator",
    };
    let order = order_map.get(template_id).copied().unwrap_or(0);
    let binding_revision = raw
        .get("bindingRevision")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let enabled = raw.get("enabled").and_then(Value::as_bool).unwrap_or(true);

    let mut normalized = Map::new();
    normalized.insert(
        "templateId".to_string(),
        Value::String(template_id.to_string()),
    );
    normalized.insert(
        "modelSource".to_string(),
        Value::String(model_source.to_string()),
    );
    normalized.insert(
        "engineId".to_string(),
        Value::String(if model_source == "engine" {
            raw_engine_id.to_string()
        } else {
            String::new()
        }),
    );
    normalized.insert("enabled".to_string(), Value::Bool(enabled));
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
    let model_source = override_entry
        .get("modelSource")
        .and_then(Value::as_str)
        .unwrap_or("orchestrator");
    let engine_id = override_entry
        .get("engineId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let enabled = override_entry
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let has_ui_overrides = override_entry
        .get("uiOverrides")
        .is_some_and(|value| !value.is_null());
    let has_profile_overrides = override_entry
        .get("profileOverrides")
        .is_some_and(|value| !value.is_null());
    enabled
        && model_source == "orchestrator"
        && engine_id.is_empty()
        && !has_ui_overrides
        && !has_profile_overrides
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

pub(crate) fn enabled_registry_agent_roles(state: &ApiState) -> Vec<String> {
    resolve_registry_agents(state)
        .into_iter()
        .filter(|entry| {
            entry
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })
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
    _query: Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    Json(state.settings_snapshot_json())
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
    state
        .settings_store
        .set_section("safeguardConfig", scoped_settings_section_request(&request));
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
    let raw_request = request.get("agent").unwrap_or(&request);
    let requested_model_source = raw_request
        .get("modelSource")
        .and_then(Value::as_str)
        .unwrap_or("");
    let requested_engine_id = raw_request
        .get("engineId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if requested_model_source == "engine" && requested_engine_id.is_empty() {
        return Err(ApiError::InvalidInput(
            "显式绑定模型时必须提供 engineId".to_string(),
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
    let model_source = normalized
        .get("modelSource")
        .and_then(Value::as_str)
        .unwrap_or("orchestrator");
    let engine_id = normalized
        .get("engineId")
        .and_then(Value::as_str)
        .unwrap_or("");

    if model_source == "engine" {
        if engine_id.is_empty() {
            return Err(ApiError::InvalidInput(
                "显式绑定模型时必须提供 engineId".to_string(),
            ));
        }
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
    let url = config.openai_models_url()?;
    let now = UtcMillis::now();
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let auth_value = HeaderValue::from_str(&format!("Bearer {}", config.require_api_key()?))
        .map_err(|_| ApiError::InvalidInput("apiKey 包含非法字符".to_string()))?;
    headers.insert(AUTHORIZATION, auth_value);

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
        "enableThinking": model.enable_thinking,
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
    use magi_tool_runtime::ToolRegistry;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;

    fn test_state() -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let governance = Arc::new(GovernanceService::default());
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        ApiState::new(
            "magi-test",
            event_bus,
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            governance,
        )
        .with_tool_registry(tool_registry)
    }

    #[tokio::test]
    async fn settings_bootstrap_returns_frontend_contract_sections() {
        let state = test_state();
        let session_id = SessionId::new("session-empty-contract");

        let bootstrap = settings_bootstrap(
            State(state),
            Query(HashMap::from([(
                "sessionId".to_string(),
                session_id.as_str().to_string(),
            )])),
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
        for key in [
            "repositories",
            "mcpServers",
            "builtinTools",
            "roleTemplates",
            "registryEngines",
            "registryAgents",
        ] {
            assert!(bootstrap[key].is_array(), "{key} should be an array");
        }
        let builtin_tools = bootstrap["builtinTools"]
            .as_array()
            .expect("builtin tools should be an array");
        assert_eq!(builtin_tools.len(), 21);
        assert!(
            builtin_tools
                .iter()
                .any(|tool| tool["name"] == serde_json::json!("shell_exec")),
            "builtin tools should expose shell_exec"
        );
        assert_eq!(bootstrap["userRulesConfig"], serde_json::json!({}));
        assert_eq!(
            bootstrap["runtimeSettings"]["locale"],
            serde_json::json!("zh-CN")
        );
        assert_eq!(
            bootstrap["runtimeSettings"]["deepTask"],
            serde_json::json!(false)
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

    #[test]
    fn fetch_models_config_allows_openai_compatible_gateway_provider_labels() {
        let (config, target) = parse_fetch_models_config(FetchModelsRequest {
            config: serde_json::json!({
                "provider": "anthropic",
                "baseUrl": "http://127.0.0.1:8320/",
                "apiKey": "test-key",
                "urlMode": "standard"
            }),
            target: "orch".to_string(),
        })
        .expect("openai-compatible gateways may keep their provider label");

        assert_eq!(config.provider(), "anthropic");
        assert_eq!(
            config.require_base_url().expect("baseUrl"),
            "http://127.0.0.1:8320/"
        );
        assert_eq!(config.require_api_key().expect("apiKey"), "test-key");
        config
            .require_openai_models_listable()
            .expect("standard url mode can list models");
        assert_eq!(target, "orch");
    }

    #[test]
    fn fetch_models_config_rejects_full_url_mode() {
        let error = parse_fetch_models_config(FetchModelsRequest {
            config: serde_json::json!({
                "provider": "anthropic",
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
                "baseUrl": "https://api.openai.com",
                "urlMode": "default",
                "openaiProtocol": "responses"
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
                    "category": "custom"
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
        assert!(
            bootstrap_a["safeguardConfig"]["rules"]
                .as_array()
                .expect("rules should be array")
                .iter()
                .any(|rule| rule["pattern"] == serde_json::json!("custom-danger-global"))
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
                .any(|rule| rule["pattern"] == serde_json::json!("custom-danger-global"))
        );
    }
}
