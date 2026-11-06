//! 主对话的数据源目录。会话通过 sourceId + model 引用，解析只有一条路径。
//! 目录由两种来源合并：`custom/<id>` 是用户配置，持久化在 settings 的 `orchestrator` 段；
//! `plugin/<pluginId>/<contributionId>` 是插件在本次运行中贡献的数据源，只存在于运行期 section，
//! 不写盘，插件停用、卸载或宿主退出时撤销。插件来源只追加到主对话目录，不参与其他角色。
use crate::model_config::NormalizedModelConfig;
use crate::model_context_window::{MAX_MODEL_CONTEXT_WINDOW, MIN_MODEL_CONTEXT_WINDOW};
use magi_core::model_reasoning::ReasoningCapability;
use magi_settings_store::SettingsStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MainModelEntry {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    /// 来源声明的上下文窗口（Token）。缺省表示来源未声明，由用户设置或内置规则解析，不按模型名猜测。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u64>,
    /// 来源声明的输入模态。缺省表示未声明（保持按文本规则判定）；显式列表则以它为准。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<ModelInputModality>>,
    /// 来源声明的推理能力。缺省表示未声明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningCapability>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelInputModality {
    Text,
    Image,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MainModelSource {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub api_protocol: String,
    pub models: Vec<MainModelEntry>,
}

impl MainModelSource {
    pub fn identity(&self) -> String {
        format!("custom/{}", self.id)
    }

    pub fn connection(&self) -> Value {
        json!({"baseUrl": self.base_url, "apiKey": self.api_key, "apiProtocol": self.api_protocol})
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainModelSourcesConfig {
    #[serde(default)]
    pub revision: u64,
    pub sources: Vec<MainModelSource>,
}

/// `effective_config` 输出里携带来源声明的推理能力的内部字段名，只在执行链内部使用。
pub const DECLARED_REASONING_FIELD: &str = "declaredReasoning";

/// 插件贡献来源所在的运行期 section。结构：`{ "<pluginId>": { "<contributionId>": MainModelSource } }`。
pub const PLUGIN_SOURCES_SECTION: &str = "pluginModelSources";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Custom,
    Plugin,
}

/// 目录中的一个来源。用户配置与插件贡献解析后同形，之后的投影、选择校验和连接解析都只面向它。
#[derive(Clone, Debug)]
pub struct CatalogSource {
    pub identity: String,
    pub kind: SourceKind,
    pub plugin_id: Option<String>,
    pub source: MainModelSource,
}

pub fn plugin_identity(plugin_id: &str, contribution_id: &str) -> String {
    format!("plugin/{plugin_id}/{contribution_id}")
}

/// 校验并规范化一个来源的名称、连接和模型目录（用户来源与插件来源共用）。
fn normalize_source(source: &mut MainModelSource) -> Result<(), String> {
    source.name = source.name.trim().to_string();
    source.base_url = source.base_url.trim().trim_end_matches('/').to_string();
    source.api_key = source.api_key.trim().to_string();
    if source.name.is_empty() || source.name.len() > 128 {
        return Err("数据源名称不能为空且最多 128 字节".into());
    }
    let normalized = NormalizedModelConfig::from_settings_value(&source.connection())?;
    normalized.require_base_url()?;
    // 与真实 HTTP 客户端共用地址校验，不允许在目录里保存无效连接。
    normalized.models_list_url()?;
    if source.models.len() > 2048 {
        return Err("每个数据源最多 2048 个模型".into());
    }
    let mut models = HashSet::new();
    for model in &mut source.models {
        model.id = model.id.trim().to_string();
        model.display_name = model.display_name.trim().to_string();
        if let Some(window) = model.context_window_tokens
            && !(MIN_MODEL_CONTEXT_WINDOW..=MAX_MODEL_CONTEXT_WINDOW).contains(&window)
        {
            return Err(format!(
                "模型 {} 的上下文窗口必须在 {MIN_MODEL_CONTEXT_WINDOW} 到 {MAX_MODEL_CONTEXT_WINDOW} 之间",
                model.id
            ));
        }
        if let Some(modalities) = &mut model.input_modalities {
            modalities.sort_by_key(|m| *m as u8);
            modalities.dedup();
            if !modalities.contains(&ModelInputModality::Text) {
                return Err(format!("模型 {} 的输入模态必须包含 text", model.id));
            }
        }
        if let Some(reasoning) = &mut model.reasoning {
            reasoning
                .validate()
                .map_err(|error| format!("模型 {} 的推理能力无效：{error}", model.id))?;
        }
        if model.id.is_empty()
            || model.id.len() > 512
            || model.display_name.len() > 512
            || !models.insert(model.id.clone())
        {
            return Err("同一数据源内的模型 ID 必须非空且唯一，模型名称最长 512 字节".into());
        }
    }
    Ok(())
}

pub fn parse(value: &Value) -> Result<MainModelSourcesConfig, String> {
    if value.is_null() {
        return Ok(MainModelSourcesConfig::default());
    }
    let mut config: MainModelSourcesConfig = serde_json::from_value(value.clone())
        .map_err(|_| "主对话配置必须是 sources 数组；每项包含 id、name、baseUrl、apiKey、apiProtocol 和 models".to_string())?;
    if config.sources.len() > 64 {
        return Err("主对话数据源最多 64 个".into());
    }
    let mut ids = HashSet::new();
    for source in &mut config.sources {
        source.id = source.id.trim().to_string();
        if source.id.is_empty()
            || source.id.len() > 64
            || !source.id.as_bytes()[0].is_ascii_lowercase()
            || !source
                .id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || !ids.insert(source.id.clone())
        {
            return Err(
                "数据源 ID 必须唯一，以小写字母开头，只含小写字母、数字和连字符，最长 64 位".into(),
            );
        }
        normalize_source(source)?;
    }
    Ok(config)
}

/// 用户是否配置过主对话数据源。没有任何用户数据源即“未配置”，与“配置了但选择无效”不同：
/// 未配置时由宿主注入的默认模型客户端（开发/测试/嵌入场景）承接，配置了就必须按目录解析，失败即报错。
/// 插件来源是追加项，不改变“是否配置”。
pub fn is_configured(store: &SettingsStore) -> bool {
    parse(&store.get_section("orchestrator")).is_ok_and(|config| !config.sources.is_empty())
}

fn plugin_catalog_sources(store: &SettingsStore) -> Vec<CatalogSource> {
    let section = store.get_runtime_section(PLUGIN_SOURCES_SECTION);
    let mut sources = Vec::new();
    if let Some(plugins) = section.as_object() {
        for (plugin_id, contributions) in plugins {
            for (contribution_id, value) in contributions.as_object().into_iter().flatten() {
                // 写入时已校验；读取不再信任之外的形状。
                if let Ok(source) = serde_json::from_value::<MainModelSource>(value.clone()) {
                    sources.push(CatalogSource {
                        identity: plugin_identity(plugin_id, contribution_id),
                        kind: SourceKind::Plugin,
                        plugin_id: Some(plugin_id.clone()),
                        source,
                    });
                }
            }
        }
    }
    sources.sort_by(|left, right| left.identity.cmp(&right.identity));
    sources
}

/// 完整目录：用户来源在前（保持用户配置的顺序），插件来源追加在后。
pub fn sources(store: &SettingsStore) -> Result<Vec<CatalogSource>, String> {
    let mut all = parse(&store.get_section("orchestrator"))?
        .sources
        .into_iter()
        .map(|source| CatalogSource {
            identity: source.identity(),
            kind: SourceKind::Custom,
            plugin_id: None,
            source,
        })
        .collect::<Vec<_>>();
    all.extend(plugin_catalog_sources(store));
    Ok(all)
}

/// 插件提交（或整体替换）自己的一个数据源。`name` 来自清单中的贡献标题，其余来自插件运行时。
pub fn set_plugin_source(
    store: &SettingsStore,
    plugin_id: &str,
    contribution_id: &str,
    name: &str,
    input: &Value,
) -> Result<(), String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Input {
        base_url: String,
        api_key: String,
        api_protocol: String,
        models: Vec<MainModelEntry>,
    }
    let input: Input = serde_json::from_value(input.clone())
        .map_err(|_| "模型数据源需要 baseUrl、apiKey、apiProtocol 和 models".to_string())?;
    let mut source = MainModelSource {
        id: contribution_id.to_string(),
        name: name.to_string(),
        base_url: input.base_url,
        api_key: input.api_key,
        api_protocol: input.api_protocol,
        models: input.models,
    };
    normalize_source(&mut source)?;
    let value = serde_json::to_value(&source).map_err(|_| "模型数据源序列化失败".to_string())?;
    store.update_runtime_section(PLUGIN_SOURCES_SECTION, |section| {
        if !section.is_object() {
            *section = json!({});
        }
        section[plugin_id][contribution_id] = value;
    });
    Ok(())
}

/// 撤销插件的一个数据源；不存在时无操作。
pub fn clear_plugin_source(store: &SettingsStore, plugin_id: &str, contribution_id: &str) {
    store.update_runtime_section(PLUGIN_SOURCES_SECTION, |section| {
        if let Some(contributions) = section.get_mut(plugin_id).and_then(Value::as_object_mut) {
            contributions.remove(contribution_id);
            if contributions.is_empty() {
                section
                    .as_object_mut()
                    .map(|plugins| plugins.remove(plugin_id));
            }
        }
        if section
            .as_object()
            .is_some_and(|plugins| plugins.is_empty())
        {
            *section = Value::Null;
        }
    });
}

/// 撤销插件的全部数据源（插件停用、卸载、崩溃或宿主退出时）。
pub fn clear_plugin_sources(store: &SettingsStore, plugin_id: &str) {
    if store
        .get_runtime_section(PLUGIN_SOURCES_SECTION)
        .get(plugin_id)
        .is_none()
    {
        return;
    }
    store.update_runtime_section(PLUGIN_SOURCES_SECTION, |section| {
        if let Some(plugins) = section.as_object_mut() {
            plugins.remove(plugin_id);
        }
        if section
            .as_object()
            .is_some_and(|plugins| plugins.is_empty())
        {
            *section = Value::Null;
        }
    });
}

/// 当设置中还没有明确的主模型选择时，投影用户数据源目录中的第一个模型。
/// 插件来源只追加、不会成为隐式默认。
/// API bootstrap 与 daemon 运行时共用这条规则，避免 UI 看见可用选择但执行端仍视为未配置。
pub fn with_first_model_default(store: &SettingsStore, selection: &Value) -> Value {
    let has_selection = selection
        .get("sourceId")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
        && selection
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty());
    if has_selection {
        return selection.clone();
    }

    let Ok(sources) = parse(&store.get_section("orchestrator")) else {
        return selection.clone();
    };
    let Some((source_id, model_id)) = sources.sources.iter().find_map(|source| {
        source
            .models
            .first()
            .map(|model| (source.identity(), model.id.clone()))
    }) else {
        return selection.clone();
    };

    let mut resolved = selection
        .as_object()
        .cloned()
        .map(Value::Object)
        .unwrap_or_default();
    resolved["sourceId"] = json!(source_id);
    resolved["model"] = json!(model_id);
    resolved["reasoningEffort"] = Value::Null;
    resolved
}

/// 选中来源声明的输入模态：`Some(true/false)` 表示来源明确声明了是否接受图片，`None` 表示未声明。
pub fn declared_image_input(store: &SettingsStore, source_id: &str, model: &str) -> Option<bool> {
    let entry = sources(store).ok()?.into_iter().find_map(|candidate| {
        (candidate.identity == source_id).then(|| {
            candidate
                .source
                .models
                .into_iter()
                .find(|entry| entry.id == model)
        })
    })??;
    entry
        .input_modalities
        .map(|modalities| modalities.contains(&ModelInputModality::Image))
}

pub fn effective_config(store: &SettingsStore, selection: &Value) -> Result<Value, String> {
    let identity = selection
        .get("sourceId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("主对话尚未选择数据源，请选择数据源中的模型")?;
    let missing = || {
        if identity.starts_with("custom/") {
            "会话使用的数据源已移除，请重新选择模型"
        } else if identity.starts_with("plugin/") {
            "会话使用的插件数据源当前不可用（插件已停用或尚未提供），请重新选择模型"
        } else {
            "当前不支持此模型数据源类型"
        }
    };
    let CatalogSource { source, .. } = sources(store)?
        .into_iter()
        .find(|candidate| candidate.identity == identity)
        .ok_or_else(missing)?;
    let model = selection
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("主对话尚未选择模型")?;
    if !source.models.iter().any(|entry| entry.id == model) {
        return Err("会话使用的模型已从数据源目录移除，请重新选择模型".into());
    }
    let entry = source
        .models
        .iter()
        .find(|entry| entry.id == model)
        .ok_or("会话使用的模型已从数据源目录移除，请重新选择模型")?;
    let mut config = source.connection();
    config["sourceId"] = json!(identity);
    config["model"] = json!(model);
    // 用户在设置里明确指定的窗口优先；其次是来源声明的窗口；都没有才回到内置规则。
    if let Some(context_window) =
        crate::model_context_window::configured_main_source_model_context_window(
            Some(store),
            identity,
            model,
        )
        .or(entry.context_window_tokens)
    {
        config["contextWindowTokens"] = json!(context_window);
    }
    // 来源声明的推理能力随选择一并交给归一化：声明优先于缓存的目录探测结果。
    if let Some(reasoning) = &entry.reasoning {
        config[DECLARED_REASONING_FIELD] = json!(reasoning);
    }
    config["reasoningEffort"] = selection
        .get("reasoningEffort")
        .cloned()
        .unwrap_or(Value::Null);
    Ok(config)
}

/// 目录投影只暴露身份、名称和能力，不含连接地址或密钥。用户来源与插件来源同形，以 `kind` 区分。
pub fn catalog(store: &SettingsStore) -> Result<Value, String> {
    let groups = sources(store)?
        .into_iter()
        .map(|entry| {
            let CatalogSource {
                identity,
                kind,
                plugin_id,
                source,
            } = entry;
            let config =
                NormalizedModelConfig::from_settings_with_catalog(store, &source.connection())?;
            let models = source
                .models
                .iter()
                .map(|model| {
                    let mut projected = json!({
                        "id": model.id,
                        "displayName": if model.display_name.is_empty() { &model.id } else { &model.display_name },
                        "reasoningCapability": model
                            .reasoning
                            .clone()
                            .unwrap_or_else(|| config.reasoning_capability(&model.id)),
                    });
                    if let Some(window) = model.context_window_tokens {
                        projected["contextWindowTokens"] = json!(window);
                    }
                    if let Some(modalities) = &model.input_modalities {
                        projected["inputModalities"] = json!(modalities);
                    }
                    projected
                })
                .collect::<Vec<_>>();
            let mut group = json!({
                "sourceId": identity,
                "name": source.name,
                "kind": match kind { SourceKind::Custom => "custom", SourceKind::Plugin => "plugin" },
                "models": models,
            });
            if let Some(plugin_id) = plugin_id {
                group["pluginId"] = json!(plugin_id);
            }
            Ok(group)
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(json!({"groups": groups}))
}

/// 测试夹具：单个 `custom/test` 数据源。选择时使用 [`TEST_SOURCE_IDENTITY`]。
#[cfg(test)]
pub(crate) fn test_sources(base_url: &str, api_key: &str, models: &[&str]) -> Value {
    json!({"revision": 1, "sources": [{
        "id": "test",
        "name": "test",
        "baseUrl": base_url,
        "apiKey": api_key,
        "apiProtocol": "openai_chat",
        "models": models.iter().map(|model| json!({"id": model})).collect::<Vec<_>>(),
    }]})
}

#[cfg(test)]
pub(crate) const TEST_SOURCE_IDENTITY: &str = "custom/test";

#[cfg(test)]
mod tests {
    use super::*;
    fn source(id: &str, key: &str) -> Value {
        json!({"id":id,"name":id,"baseUrl":"https://example.test/v1","apiKey":key,"apiProtocol":"openai_chat","models":[{"id":"same-model"}]})
    }
    #[test]
    fn same_model_is_resolved_with_its_own_source_and_catalog_hides_secrets() {
        let store = SettingsStore::new();
        store
            .set_section(
                "orchestrator",
                json!({"sources":[source("one","secret-one"),source("two","secret-two")]}),
            )
            .unwrap();
        let selected = json!({"sourceId":"custom/two","model":"same-model"});
        let resolved = effective_config(&store, &selected).unwrap();
        assert_eq!(resolved["apiKey"], "secret-two");
        assert_eq!(resolved["sourceId"], "custom/two");
        crate::model_context_window::set_main_source_model_context_window(
            &store,
            "custom/two",
            "same-model",
            512_000,
        )
        .unwrap();
        assert_eq!(
            effective_config(&store, &selected).unwrap()["contextWindowTokens"],
            512_000
        );
        let projection = catalog(&store).unwrap().to_string();
        assert!(!projection.contains("secret"));
        assert!(!projection.contains("example.test"));
        assert!(
            effective_config(
                &store,
                &json!({"sourceId":"custom/missing","model":"same-model"})
            )
            .is_err()
        );
        assert!(
            effective_config(
                &store,
                &json!({"sourceId":"plugin/owner/source","model":"same-model"})
            )
            .is_err(),
            "没有对应插件贡献时 plugin/ 身份不可用"
        );
        assert!(effective_config(&store, &json!({"model":"same-model"})).is_err());
    }
    fn plugin_input(models: &[&str]) -> Value {
        json!({
            "baseUrl": "https://plugin.example.test/v1", "apiKey": "plugin-secret",
            "apiProtocol": "openai_chat",
            "models": models.iter().map(|id| json!({"id": id})).collect::<Vec<_>>(),
        })
    }
    #[test]
    fn plugin_sources_are_appended_resolved_and_projected_like_custom_ones() {
        let store = SettingsStore::new();
        store
            .set_section(
                "orchestrator",
                json!({"sources":[source("one","secret-one")]}),
            )
            .unwrap();
        set_plugin_source(
            &store,
            "acme.proxy",
            "gateway",
            "Gateway",
            &plugin_input(&["p1"]),
        )
        .unwrap();
        // 用户来源在前，插件来源追加在后；二者同形。
        let identities = sources(&store)
            .unwrap()
            .into_iter()
            .map(|s| s.identity)
            .collect::<Vec<_>>();
        assert_eq!(identities, ["custom/one", "plugin/acme.proxy/gateway"]);
        let selected = json!({"sourceId":"plugin/acme.proxy/gateway","model":"p1"});
        let resolved = effective_config(&store, &selected).unwrap();
        assert_eq!(resolved["apiKey"], "plugin-secret");
        assert_eq!(resolved["sourceId"], "plugin/acme.proxy/gateway");
        // 目录投影区分来源类型，且不泄露连接信息。
        let projection = catalog(&store).unwrap();
        assert_eq!(projection["groups"][1]["kind"], "plugin");
        assert_eq!(projection["groups"][1]["pluginId"], "acme.proxy");
        assert!(!projection.to_string().contains("plugin-secret"));
        assert!(!projection.to_string().contains("plugin.example.test"));
        // 插件来源不会成为隐式默认，也不改变“是否配置”。
        let only_plugin = SettingsStore::new();
        set_plugin_source(
            &only_plugin,
            "acme.proxy",
            "gateway",
            "Gateway",
            &plugin_input(&["p1"]),
        )
        .unwrap();
        assert!(!is_configured(&only_plugin));
        assert_eq!(
            with_first_model_default(&only_plugin, &json!({})),
            json!({})
        );
        assert_eq!(
            with_first_model_default(&store, &json!({}))["sourceId"],
            "custom/one"
        );
        // 执行快照冻结提交时刻的目录；之后的撤销不影响已接受的执行。
        let frozen = store.execution_snapshot();
        clear_plugin_source(&store, "acme.proxy", "gateway");
        assert!(effective_config(&frozen, &selected).is_ok());
        let error = effective_config(&store, &selected).unwrap_err();
        assert!(error.contains("插件数据源当前不可用"), "{error}");
    }
    fn declared_input(models: Value) -> Value {
        json!({
            "baseUrl": "https://plugin.example.test/v1", "apiKey": "plugin-secret",
            "apiProtocol": "openai_chat", "models": models,
        })
    }
    #[test]
    fn declared_capabilities_flow_into_resolution_and_projection() {
        let store = SettingsStore::new();
        set_plugin_source(
            &store,
            "acme.proxy",
            "gateway",
            "Gateway",
            &declared_input(json!([
                {
                    "id": "vision-1", "contextWindowTokens": 200_000,
                    "inputModalities": ["image", "text"],
                    "reasoning": {"supportedEfforts": ["low", "high"], "defaultEffort": "low", "transport": "openai", "source": "plugin"}
                },
                {"id": "plain"}
            ])),
        )
        .unwrap();
        let identity = "plugin/acme.proxy/gateway";
        let selected = json!({"sourceId": identity, "model": "vision-1"});
        let resolved = effective_config(&store, &selected).unwrap();
        assert_eq!(resolved["contextWindowTokens"], 200_000);
        let config = NormalizedModelConfig::from_settings_with_catalog(&store, &resolved).unwrap();
        let capability = config.reasoning_capability("vision-1");
        assert_eq!(capability.supported_efforts.len(), 2);
        assert_eq!(capability.source, "plugin");
        // 用户在设置里明确指定的窗口优先于来源声明。
        crate::model_context_window::set_main_source_model_context_window(
            &store, identity, "vision-1", 300_000,
        )
        .unwrap();
        assert_eq!(
            effective_config(&store, &selected).unwrap()["contextWindowTokens"],
            300_000
        );
        // 声明了模态的条目以声明为准；未声明的条目返回 None，交还文本模型规则。
        assert_eq!(declared_image_input(&store, identity, "vision-1"), Some(true));
        assert_eq!(declared_image_input(&store, identity, "plain"), None);
        let projection = catalog(&store).unwrap();
        let models = &projection["groups"][0]["models"];
        assert_eq!(models[0]["contextWindowTokens"], 200_000);
        assert_eq!(models[0]["inputModalities"], json!(["text", "image"]));
        assert!(models[1].get("contextWindowTokens").is_none());
        assert_eq!(
            effective_config(
                &store,
                &json!({"sourceId": identity, "model": "plain"})
            )
            .unwrap()
            .get("contextWindowTokens"),
            None,
            "未声明窗口不得被编造"
        );
    }
    #[test]
    fn declared_capabilities_are_rejected_when_invalid() {
        let store = SettingsStore::new();
        let attempt = |model: Value| {
            set_plugin_source(&store, "acme.proxy", "gateway", "Gateway", &declared_input(json!([model])))
        };
        assert!(attempt(json!({"id": "m", "contextWindowTokens": 1000})).is_err());
        assert!(attempt(json!({"id": "m", "inputModalities": ["image"]})).is_err());
        assert!(attempt(json!({"id": "m", "unknownField": 1})).is_err());
        assert!(
            attempt(json!({"id": "m", "reasoning": {"supportedEfforts": ["low"], "defaultEffort": "high", "transport": "openai"}})).is_err()
        );
        // 传输方式与协议不一致必须在解析时报错，而不是静默忽略。
        attempt(json!({"id": "m", "reasoning": {"supportedEfforts": ["low"], "defaultEffort": "low", "transport": "anthropic_effort"}}))
            .unwrap();
        let resolved = effective_config(
            &store,
            &json!({"sourceId": "plugin/acme.proxy/gateway", "model": "m"}),
        )
        .unwrap();
        assert!(NormalizedModelConfig::from_settings_with_catalog(&store, &resolved).is_err());
    }
    #[test]
    fn plugin_sources_are_validated_replaced_and_revoked_per_plugin() {
        let store = SettingsStore::new();
        let mut bad = plugin_input(&["p1"]);
        bad["baseUrl"] = json!("not a url");
        assert!(set_plugin_source(&store, "acme.a", "s", "S", &bad).is_err());
        assert!(set_plugin_source(&store, "acme.a", "s", "S", &json!({"baseUrl": "x"})).is_err());
        assert!(
            set_plugin_source(&store, "acme.a", "s", "S", &plugin_input(&["dup", "dup"])).is_err()
        );
        assert!(sources(&store).unwrap().is_empty());
        set_plugin_source(&store, "acme.a", "s", "S", &plugin_input(&["old"])).unwrap();
        set_plugin_source(&store, "acme.a", "s", "S", &plugin_input(&["new"])).unwrap();
        set_plugin_source(&store, "acme.b", "s", "S", &plugin_input(&["other"])).unwrap();
        let models = |store: &SettingsStore| {
            sources(store)
                .unwrap()
                .into_iter()
                .map(|s| (s.identity, s.source.models.len()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            models(&store),
            [
                ("plugin/acme.a/s".to_string(), 1),
                ("plugin/acme.b/s".to_string(), 1)
            ]
        );
        assert!(
            effective_config(&store, &json!({"sourceId":"plugin/acme.a/s","model":"old"})).is_err()
        );
        let revision = store.revision();
        clear_plugin_sources(&store, "acme.a");
        assert!(store.revision() > revision, "撤销必须使执行缓存失效");
        assert_eq!(models(&store), [("plugin/acme.b/s".to_string(), 1)]);
        // 重复撤销是无操作，不推进代际。
        let revision = store.revision();
        clear_plugin_sources(&store, "acme.a");
        assert_eq!(store.revision(), revision);
        clear_plugin_sources(&store, "acme.b");
        assert!(store.get_runtime_section(PLUGIN_SOURCES_SECTION).is_null());
    }
    #[test]
    fn rejects_duplicate_sources_models_and_old_connection_shape() {
        assert!(parse(&json!({"sources":[source("one","a"),source("one","b")]})).is_err());
        let mut value = source("one", "a");
        value["models"] = json!([{"id":"m"},{"id":"m"}]);
        assert!(parse(&json!({"sources":[value]})).is_err());
        assert!(
            parse(
                &json!({"baseUrl":"https://old.test","apiKey":"key","apiProtocol":"openai_chat"})
            )
            .is_err()
        );
    }
}
