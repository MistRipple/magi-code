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
    DEFAULT_ORCHESTRATOR_REASONING_EFFORT, DEFAULT_VISION_CONTEXT_WINDOW, NormalizedModelConfig,
    VISION_MODEL_SECTION, builtin_text_model_rule_catalog, merge_orchestrator_session_override,
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
    // 引擎绑定（A22）：会话级 `engineId` 是「继承 vs 显式」的唯一字段。
    // 空串是合法的显式「继承编排模型」写法，必须原样保留，不能当无效输入拒绝；
    // 否则选择器无法把会话从 Web 引擎切回 provider 模型。
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
    // GPT Web 对话方式（临时 / 已保存）：只在会话首条消息之前可以设置，落到会话级
    // `webConversation` 绑定，不进入 orchestrator section（避免第二份事实）。
    if let Some(mode) = config.get("webMode") {
        match mode.as_str().map(str::trim) {
            Some(mode @ ("temporary" | "saved")) => {
                override_config.insert("webMode".to_string(), Value::String(mode.to_string()));
            }
            _ => {
                return Err(ApiError::InvalidInput(
                    "webMode 只能是 temporary 或 saved".to_string(),
                ));
            }
        }
    }
    if config.contains_key("reasoningEffort") {
        match config.get("reasoningEffort") {
            Some(Value::Null) => {
                override_config.insert(
                    "reasoningEffort".to_string(),
                    Value::String(DEFAULT_ORCHESTRATOR_REASONING_EFFORT.to_string()),
                );
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

fn ensure_session_reasoning_effort(config: &mut Value) {
    if !config.is_object() {
        *config = json!({});
    }
    let fields = config
        .as_object_mut()
        .expect("orchestrator session config should be object");
    let reasoning_effort_is_valid = fields
        .get("reasoningEffort")
        .and_then(Value::as_str)
        .is_some_and(|value| matches!(value.trim(), "low" | "medium" | "high" | "xhigh"));
    if !reasoning_effort_is_valid {
        fields.insert(
            "reasoningEffort".to_string(),
            Value::String(DEFAULT_ORCHESTRATOR_REASONING_EFFORT.to_string()),
        );
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
    save_orchestrator_session_override_for_session_with_policy(state, session_id, config, false)
}

/// 保存刚创建会话的首条请求所携带的模型绑定。
///
/// 任务接纳会先写入 canonical 用户消息，再在异步 preparation 阶段保存初始
/// session 配置。此时普通方向门禁会看到“刚刚写入的第一条消息”，误判为本地
/// 会话转 Web。调用方必须已经证明这是本次请求创建的新会话；该入口不能用于
/// 修改已有会话。
pub(crate) fn save_initial_orchestrator_session_override_for_new_session(
    state: &ApiState,
    session_id: &SessionId,
    config: &Value,
) -> Result<Option<Value>, ApiError> {
    save_orchestrator_session_override_for_session_with_policy(state, session_id, config, true)
}

fn save_orchestrator_session_override_for_session_with_policy(
    state: &ApiState,
    session_id: &SessionId,
    config: &Value,
    allow_new_session_initial_web_binding: bool,
) -> Result<Option<Value>, ApiError> {
    let override_config = orchestrator_session_override_request(config)?;
    let Some(override_fields) = override_config.as_object() else {
        return Ok(None);
    };
    let mut override_fields = override_fields.clone();
    let web_mode = override_fields
        .remove("webMode")
        .and_then(|value| value.as_str().map(ToOwned::to_owned));
    if let Some(mode) = web_mode.as_deref() {
        state.set_web_conversation_mode(
            session_id,
            mode,
            &override_fields,
            allow_new_session_initial_web_binding,
        )?;
    }
    if override_fields.is_empty() {
        return Ok(None);
    }
    let override_fields = &override_fields;

    // 会话转换是单向的（设计基线 A26）：空白会话可以启动 GPT Web；已有本地
    // canonical 历史的会话不能再把历史导入网页端。Web 会话切回 provider 不会
    // 进入这里的 Web 分支，因此自然保留 canonical 历史并清除 engineId。
    ensure_web_session_direction_allowed(
        state,
        session_id,
        override_fields,
        allow_new_session_initial_web_binding,
    )?;

    let mut next_config = state
        .settings_store
        .get_session_section(session_id, "orchestrator");
    let was_web = next_config
        .get("engineId")
        .and_then(Value::as_str)
        .is_some_and(magi_web_model::is_chatgpt_web_engine_id);
    if !next_config.is_object() {
        next_config = json!({});
    }
    let next_fields = next_config
        .as_object_mut()
        .expect("session orchestrator config should be object");
    for (key, value) in override_fields {
        next_fields.insert(key.clone(), value.clone());
    }
    ensure_session_reasoning_effort(&mut next_config);
    let mut next_defaults = orchestrator_session_defaults(state);
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
    let now_web = next_config
        .get("engineId")
        .and_then(Value::as_str)
        .is_some_and(magi_web_model::is_chatgpt_web_engine_id);
    if was_web && !now_web {
        // Web → 本地：槽位随会话切走立即释放（W11），本地历史保留。
        let state = state.clone();
        let session_id = session_id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                state.release_web_slot_for_session(&session_id).await;
            });
        }
    }
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

/// 拒绝「已有本地历史 → GPT Web」的会话迁移。
///
/// 只把真正的 Web 引擎绑定视为方向切换；清空 `engineId`（Web → provider）
/// 不受限制。空白会话也不受限制，因为它会在首条消息时成为新的 GPT Web 会话。
fn ensure_web_session_direction_allowed(
    state: &ApiState,
    session_id: &SessionId,
    override_fields: &Map<String, Value>,
    allow_new_session_initial_web_binding: bool,
) -> Result<(), ApiError> {
    let Some(requested_engine_id) = override_fields
        .get("engineId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };

    // GPT Web 是固定入口 `plugin/openai.chatgpt-web/default`：只按引擎 id 命名空间判定，不查引擎注册表。
    let is_web_engine_id = magi_web_model::is_chatgpt_web_engine_id;
    let requested_is_web = is_web_engine_id(requested_engine_id);
    if !requested_is_web {
        return Ok(());
    }

    // Web → Web（换模型 / effort）仍是 Web 会话内部操作，不属于本地 → Web
    // 转换；只有当前会话尚未使用 Web 时才需要拦截。
    let current_engine_id = state
        .settings_store
        .get_session_section(session_id, "orchestrator")
        .get("engineId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let current_is_web = current_engine_id.as_deref().is_some_and(is_web_engine_id);
    if current_is_web {
        return Ok(());
    }

    if allow_new_session_initial_web_binding {
        return Ok(());
    }

    let has_canonical_user_history = state
        .session_store
        .session(session_id)
        .and_then(|session| session.message_count)
        .is_some_and(|message_count| message_count > 0);
    if has_canonical_user_history {
        return Err(ApiError::InvalidInput(
            "当前本地会话已有历史，暂不支持转为 GPT Web；请新建 GPT Web 会话。GPT Web 会话可以切回本地模型并保留历史".to_string(),
        ));
    }
    Ok(())
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

/// 判定某个引擎条目是否是 GPT Web 引擎（`apiProtocol = chatgpt_web`）。
pub(crate) fn is_chatgpt_web_engine(entry: &Value) -> bool {
    entry
        .get("apiProtocol")
        .and_then(Value::as_str)
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("chatgpt_web"))
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
    if is_chatgpt_web_engine(request) {
        // GPT Web 入口由登录状态自动提供（固定的 `plugin/openai.chatgpt-web/default`），不写入引擎注册表：
        // 注册表里的条目都是 HTTP 引擎，Web 不是第二种“模型”。
        return Err(ApiError::InvalidInput(
            "GPT Web 入口由登录状态自动提供，不能写入模型引擎注册表".to_string(),
        ));
    }
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
                "urlMode": "standard",
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
