//! 模型配置解析与会话级配置策略；持久化和更新事件由同一入口负责。

use crate::{
    errors::{ApiError, settings_persistence_error},
    state::ApiState,
};
use magi_core::{EventId, SessionId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_settings_store::ORCHESTRATOR_SESSION_DEFAULTS_SECTION;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

pub(crate) use magi_conversation_runtime::model_config::{
    DEFAULT_VISION_CONTEXT_WINDOW, NormalizedModelConfig, VISION_MODEL_SECTION,
    builtin_text_model_rule_catalog, merge_orchestrator_session_override,
    reject_unknown_model_config_fields, resolve_orchestrator_model_config,
    validate_vision_model_settings,
};

fn orchestrator_session_override_request(config: &Value) -> Result<Value, ApiError> {
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
    if config.contains_key("engineId") {
        match config.get("engineId") {
            Some(Value::String(value)) => {
                override_config.insert(
                    "engineId".to_string(),
                    Value::String(value.trim().to_string()),
                );
            }
            Some(_) => {
                return Err(ApiError::InvalidInput("engineId 必须是字符串".to_string()));
            }
            None => {}
        }
    }

    if config.contains_key("reasoningEffort") {
        match config.get("reasoningEffort") {
            Some(Value::Null) => {
                override_config.insert("reasoningEffort".to_string(), Value::Null);
            }
            Some(Value::String(value)) => {
                let value = value.trim();
                if !matches!(
                    value,
                    "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
                ) {
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

fn ensure_session_reasoning_effort(config: &mut Value) {
    if !config.is_object() {
        *config = json!({});
    }
    let fields = config
        .as_object_mut()
        .expect("orchestrator session config should be object");
    let reasoning_effort_is_valid = fields.get("reasoningEffort") == Some(&Value::Null)
        || fields
            .get("reasoningEffort")
            .and_then(Value::as_str)
            .is_some_and(|value| {
                matches!(
                    value.trim(),
                    "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
                )
            });
    if !reasoning_effort_is_valid {
        fields.insert("reasoningEffort".to_string(), Value::Null);
    }
}

pub(crate) fn orchestrator_session_defaults(state: &ApiState) -> Value {
    let stored = state
        .settings_store
        .get_section(ORCHESTRATOR_SESSION_DEFAULTS_SECTION);
    let mut defaults = orchestrator_session_override_request(&stored).unwrap_or_else(|_| json!({}));
    ensure_session_reasoning_effort(&mut defaults);
    defaults
}

pub(crate) fn resolved_orchestrator_session_config(
    state: &ApiState,
    session_id: &SessionId,
) -> Value {
    let mut config = orchestrator_session_defaults(state);
    let session_override = state
        .settings_store
        .get_session_section(session_id, "orchestrator");
    merge_orchestrator_session_override(&mut config, &session_override);
    config
}

pub(crate) fn ensure_orchestrator_session_defaults_from_models(
    state: &ApiState,
    models: &[String],
) -> Result<Value, ApiError> {
    let first_model = models.first().cloned();
    state
        .settings_store
        .update_section(ORCHESTRATOR_SESSION_DEFAULTS_SECTION, |stored| {
            let mut defaults =
                orchestrator_session_override_request(stored).unwrap_or_else(|_| json!({}));
            ensure_session_reasoning_effort(&mut defaults);
            let fields = defaults
                .as_object_mut()
                .expect("orchestrator session defaults should be object");
            let has_model = fields
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty());
            if !has_model && let Some(model) = first_model.as_ref() {
                fields.insert("model".to_string(), Value::String(model.clone()));
                fields.insert("reasoningEffort".into(), Value::Null);
            }
            *stored = defaults;
        })
        .map_err(settings_persistence_error)?;
    Ok(orchestrator_session_defaults(state))
}

pub(crate) fn save_orchestrator_session_override_for_session(
    state: &ApiState,
    session_id: &SessionId,
    config: &Value,
) -> Result<Option<Value>, ApiError> {
    let override_config = orchestrator_session_override_request(config)?;
    let Some(override_fields) = override_config.as_object() else {
        return Ok(None);
    };
    let override_fields = override_fields.clone();

    if override_fields.is_empty() {
        return Ok(None);
    }
    let override_fields = &override_fields;

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
    let requested = next_config
        .get("reasoningEffort")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    ensure_session_reasoning_effort(&mut next_config);
    let mut effective = state.settings_store.get_section("orchestrator");
    merge_orchestrator_session_override(&mut effective, &next_config);
    if !next_config
        .get("engineId")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
        && let Ok(resolved) =
            NormalizedModelConfig::from_settings_with_catalog(&state.settings_store, &effective)
        && let Ok(model) = resolved.require_model()
    {
        let capability = resolved.reasoning_capability(model);
        if capability.source != "unknown" {
            // null is the user's choice to follow the model default, not a concrete effort.
            next_config["reasoningEffort"] =
                json!(requested.filter(|value| capability.supported_efforts.contains(value)));
        }
    }

    let mut next_defaults = orchestrator_session_defaults(state);
    // Web's `default` family is session-owned. Never promote it to the
    // provider defaults used by the next local conversation.
    merge_orchestrator_session_override(&mut next_defaults, &next_config);
    state
        .settings_store
        .set_session_and_global_sections(
            session_id,
            "orchestrator",
            next_config.clone(),
            ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
            next_defaults,
        )
        .map_err(settings_persistence_error)?;

    let workspace_id = state
        .session_store
        .session(session_id)
        .and_then(|session| state.session_workspace_id(&session));
    state.event_bus.publish(
        EventEnvelope::domain(
            EventId::unique(format!(
                "event-session-configuration-updated-{}",
                session_id
            )),
            "session.configuration.updated",
            json!({
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "orchestrator_session_config": next_config.clone(),
            }),
        )
        .with_context(EventContext {
            workspace_id,
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    );
    Ok(Some(next_config))
}

pub(crate) fn require_orchestrator_session_model(
    state: &ApiState,
    session_id: &SessionId,
) -> Result<(), ApiError> {
    let config = resolve_orchestrator_model_config(&state.settings_store, Some(session_id))
        .map_err(ApiError::InvalidInput)?;
    config
        .require_model()
        .map(|_| ())
        .map_err(|_| ApiError::InvalidInput("主线模型尚未选择，请刷新模型列表后重试".to_string()))
}

pub(crate) fn initial_session_orchestrator_config(
    state: &ApiState,
    created_session: bool,
    config: Option<&serde_json::Value>,
) -> Result<Option<serde_json::Value>, ApiError> {
    if !created_session && config.is_some() {
        return Err(ApiError::InvalidInput(
            "已有会话的模型只能通过会话模型设置操作修改".to_string(),
        ));
    }
    if !created_session {
        return Ok(None);
    }
    let mut initial = orchestrator_session_defaults(state);
    let has_default_model = initial
        .get("model")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if config.is_none() && !has_default_model {
        return Ok(None);
    }
    if let Some(config) = config {
        let explicit = orchestrator_session_override_request(config)?;
        magi_conversation_runtime::model_config::merge_orchestrator_session_override(
            &mut initial,
            &explicit,
        );
    }
    Ok(Some(initial))
}

/// 生成前端角色 DTO 的唯一入口。内置和用户角色都从同一个规范 AgentRole 生成，
/// API 不再维护一份独立的角色模板数组。
pub(crate) fn role_templates_for_registry(
    registry: &magi_agent_role::AgentRoleRegistry,
) -> Vec<Value> {
    registry
        .all()
        .into_iter()
        .filter(|role| registry.is_spawnable_agent_role(&role.id))
        .map(|role| {
            let capabilities = registry
                .capability_summaries_for_role(&role.id)
                .into_iter()
                .map(|capability| serde_json::to_value(capability).expect("能力摘要必须可序列化"))
                .collect::<Vec<_>>();
            json!({
                "templateId": role.id,
                "displayName": role.display_name,
                "description": role.description,
                "i18n": {
                    "displayNameKey": format!("roleTemplate.{}.displayName", role.id),
                    "descriptionKey": format!("roleTemplate.{}.description", role.id),
                },
                "source": if registry.is_builtin(&role.id) { "builtin" } else { "user" },
                "editable": !registry.is_builtin(&role.id),
                "deletable": !registry.is_builtin(&role.id),
                "version": role.version,
                "roleRevision": role.role_revision,
                "supportedKinds": role.supported_kinds,
                "parallelismLimit": role.parallelism_limit,
                "coordinatorMode": role.coordinator_mode,
                "systemPrompt": role.system_prompt,
                "defaultUI": { "colorToken": role.color_token, "icon": role.icon },
                "profile": {
                    "role": role.role,
                    "focus": role.focus,
                    "constraints": role.constraints,
                    "outputPreferences": role.output_preferences,
                },
                "ownerships": role.ownerships,
                "insightPreferences": role.insight_preferences,
                "capabilities": capabilities,
            })
        })
        .collect()
}

pub(crate) fn template_ids_for_registry(
    registry: &magi_agent_role::AgentRoleRegistry,
) -> HashSet<String> {
    role_templates_for_registry(registry)
        .iter()
        .filter_map(|template| template.get("templateId").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

pub(crate) fn template_order_map_for_registry(
    registry: &magi_agent_role::AgentRoleRegistry,
) -> HashMap<String, usize> {
    role_templates_for_registry(registry)
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

/// 返回当前真正可用的引擎投影：插件全局生效，激活的插件引擎对所有工作区一致。
pub(crate) fn load_registry_engines(state: &ApiState) -> Vec<Value> {
    let raw_engines = state.settings_store.get_section("engines");
    let mut normalized = normalize_engine_entries(&raw_engines);
    let worker_configs =
        normalize_worker_model_config_entries(&state.settings_store.get_section("workers"));
    align_engine_llm_with_worker_configs(&mut normalized, &worker_configs);
    if let Ok(manager) = state.plugin_manager.lock()
        && let Ok(contributions) =
            manager.engine_contributions_for_scope(magi_plugin_system::APPLICATION_SCOPE)
    {
        for (manifest, engine) in contributions {
            let id = magi_plugin_system::ContributionRef::new(&manifest.id, &engine.id).identity();
            if normalized
                .iter()
                .any(|entry| entry.get("id") == Some(&Value::String(id.clone())))
            {
                continue;
            }
            normalized.push(serde_json::json!({
                "id": id,
                "displayName": engine.title,
                "description": engine.description,
                "source": "plugin",
                "pluginId": manifest.id,
                "pluginContributionId": engine.id,
                "llm": {
                    "apiProtocol": "plugin",
                    "model": id,
                },
            }));
        }
    }
    normalized
}

/// 返回模型设置真正拥有的引擎。插件引擎只是插件清单的运行时投影，不能被
/// 普通模型设置的写入路径带回 `engines` 持久化段，避免卸载后留下影子配置。
fn load_user_registry_engines(state: &ApiState) -> Vec<Value> {
    let raw_engines = state.settings_store.get_section("engines");
    let mut normalized = normalize_engine_entries(&raw_engines);
    let worker_configs =
        normalize_worker_model_config_entries(&state.settings_store.get_section("workers"));
    align_engine_llm_with_worker_configs(&mut normalized, &worker_configs);
    normalized.retain(|entry| {
        entry.get("source").and_then(Value::as_str) != Some("plugin")
            && entry
                .get("id")
                .and_then(Value::as_str)
                .is_none_or(|id| !id.starts_with("plugin/"))
    });
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

pub(crate) fn load_agent_overrides(
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
    let templates = role_templates_for_registry(state.agent_role_registry.as_ref());
    let template_ids = template_ids_for_registry(state.agent_role_registry.as_ref());
    let order_map = template_order_map_for_registry(state.agent_role_registry.as_ref());
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
    state
        .agent_role_registry
        .all()
        .into_iter()
        .filter(|role| state.agent_role_registry.is_spawnable_agent_role(&role.id))
        .map(|role| role.id)
        .collect()
}

pub(crate) fn upsert_engine(state: &ApiState, request: &Value) -> Result<Value, ApiError> {
    if let Some(llm) = request.get("llm") {
        reject_unknown_model_config_fields(llm).map_err(ApiError::InvalidInput)?;
        NormalizedModelConfig::from_settings_value(llm).map_err(ApiError::InvalidInput)?;
    }
    let normalized = normalize_engine_entry(request)
        .ok_or_else(|| ApiError::InvalidInput("引擎配置缺少有效的 id".to_string()))?;
    let engine_id = normalized
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidInput("引擎配置缺少有效的 id".to_string()))?
        .to_string();
    if engine_id.starts_with("plugin/") {
        return Err(ApiError::InvalidInput(
            "插件会话引擎由插件清单管理，不能通过模型设置改写".to_string(),
        ));
    }
    let mut engines = load_user_registry_engines(state);
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
    Ok(json!({ "engines": load_registry_engines(state) }))
}

pub(crate) fn remove_engine(state: &ApiState, request: &Value) -> Result<Value, ApiError> {
    let engine_id = request
        .get("engineId")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if engine_id.starts_with("plugin/") {
        return Err(ApiError::InvalidInput(
            "插件会话引擎由插件清单管理，不能通过模型设置移除".to_string(),
        ));
    }
    let mut engines = load_user_registry_engines(state);
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
    Ok(json!({ "engines": load_registry_engines(state) }))
}

pub(crate) fn upsert_agent(state: &ApiState, request: &Value) -> Result<Value, ApiError> {
    let _configuration_guard = state
        .role_configuration_lock
        .lock()
        .map_err(|_| ApiError::InternalAssemblyError("角色配置事务锁已损坏".to_string()))?;
    let template_ids = template_ids_for_registry(state.agent_role_registry.as_ref());
    let order_map = template_order_map_for_registry(state.agent_role_registry.as_ref());
    let normalized = normalize_agent_override_entry(request, &template_ids, &order_map)
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
        let engines = load_registry_engines(state);
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

    let mut overrides = load_agent_overrides(state, &template_ids, &order_map);
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

    Ok(json!({ "agents": resolve_registry_agents(state) }))
}

pub(crate) fn remove_agent(state: &ApiState, request: &Value) -> Result<Value, ApiError> {
    let _configuration_guard = state
        .role_configuration_lock
        .lock()
        .map_err(|_| ApiError::InternalAssemblyError("角色配置事务锁已损坏".to_string()))?;
    let template_id = request
        .get("templateId")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let template_ids = template_ids_for_registry(state.agent_role_registry.as_ref());
    let order_map = template_order_map_for_registry(state.agent_role_registry.as_ref());
    let mut overrides = load_agent_overrides(state, &template_ids, &order_map);
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
    Ok(json!({ "agents": resolve_registry_agents(state) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_engine_entry_keeps_canonical_model_config() {
        let normalized = normalize_engine_entry(&json!({
            "id": "sonnet-4-5",
            "displayName": "sonnet-4.5",
            "llm": {
                "baseUrl": "http://localhost:8317/",
                "model": "kiro-claude-sonnet-4-5-agentic",
                "apiProtocol": "openai_chat"
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

    #[test]
    fn normalize_engine_entry_keeps_http_engines() {
        let http = normalize_engine_entry(&json!({
            "id": "engine-http",
            "displayName": "HTTP",
            "llm": { "baseUrl": "https://example.test", "model": "m" }
        }))
        .expect("http engine should normalize");
        assert_eq!(http["id"], json!("engine-http"));
        assert!(http.get("apiProtocol").is_none());
    }

    #[test]
    fn normalize_engine_entry_keeps_empty_llm_for_plain_engines() {
        let normalized = normalize_engine_entry(&json!({
            "id": "plain-engine",
            "displayName": "Plain",
        }))
        .expect("engine should normalize");
        assert_eq!(normalized["llm"], json!({}));
    }
}
