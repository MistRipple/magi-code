//! 代理专业能力注册表。
//!
//! 角色定义“由谁负责”，专业能力定义“怎样把某一领域的事情做好”。运行时只组合
//! 当前任务显式激活且归属于目标角色的能力，避免把所有领域提示词一次性塞给模型。

use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfessionalCapability {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub supported_roles: Vec<String>,
    pub system_prompt: String,
    pub version: u32,
}

impl ProfessionalCapability {
    fn supports_role(&self, role_id: &str) -> bool {
        self.supported_roles
            .iter()
            .any(|candidate| candidate == "*" || candidate == role_id)
    }

    fn summary(&self) -> ProfessionalCapabilitySummary {
        ProfessionalCapabilitySummary {
            id: self.id.clone(),
            display_name: self.display_name.clone(),
            description: self.description.clone(),
            version: self.version,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProfessionalCapabilitySummary {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub version: u32,
}

#[derive(Clone, Debug, Default)]
pub struct ProfessionalCapabilityRegistry {
    capabilities: Arc<HashMap<String, ProfessionalCapability>>,
}

impl ProfessionalCapabilityRegistry {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn builtin() -> Self {
        Self::from_map(builtin_capabilities_map())
    }

    pub fn load_default() -> Self {
        let mut capabilities = builtin_capabilities_map();
        if let Some(dir) = user_capability_dir()
            && dir.exists()
        {
            match load_dir(&dir) {
                Ok(overrides) => {
                    for capability in overrides {
                        capabilities.insert(capability.id.clone(), capability);
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, dir = %dir.display(), "加载专业能力覆盖失败，继续使用内置能力");
                }
            }
        }
        Self::from_map(capabilities)
    }

    pub fn from_map(capabilities: HashMap<String, ProfessionalCapability>) -> Self {
        Self {
            capabilities: Arc::new(capabilities),
        }
    }

    pub fn get(&self, capability_id: &str) -> Option<&ProfessionalCapability> {
        self.capabilities.get(capability_id)
    }

    pub fn summaries(&self) -> Vec<ProfessionalCapabilitySummary> {
        let mut capabilities = self
            .capabilities
            .values()
            .map(ProfessionalCapability::summary)
            .collect::<Vec<_>>();
        capabilities.sort_by(|left, right| left.id.cmp(&right.id));
        capabilities
    }

    pub fn ids(&self) -> Vec<String> {
        self.summaries()
            .into_iter()
            .map(|capability| capability.id)
            .collect()
    }

    pub fn summaries_for_role(&self, role_id: &str) -> Vec<ProfessionalCapabilitySummary> {
        let mut capabilities = self
            .capabilities
            .values()
            .filter(|capability| capability.supports_role(role_id))
            .map(ProfessionalCapability::summary)
            .collect::<Vec<_>>();
        capabilities.sort_by(|left, right| left.id.cmp(&right.id));
        capabilities
    }

    pub fn ids_for_role(&self, role_id: &str) -> Vec<String> {
        self.summaries_for_role(role_id)
            .into_iter()
            .map(|capability| capability.id)
            .collect()
    }

    pub fn validate_ids_for_role(
        &self,
        role_id: &str,
        capability_ids: &[String],
    ) -> Result<Vec<String>, String> {
        if capability_ids.is_empty() {
            return Err("代理任务必须至少激活一项专业能力".to_string());
        }

        let mut normalized = Vec::new();
        let mut seen = HashSet::new();
        for raw_id in capability_ids {
            let capability_id = raw_id.trim();
            if capability_id.is_empty() {
                return Err("专业能力 id 不能为空".to_string());
            }
            let capability = self
                .get(capability_id)
                .ok_or_else(|| format!("专业能力不存在: {capability_id}"))?;
            if !capability.supports_role(role_id) {
                return Err(format!("代理角色 {role_id} 不拥有专业能力 {capability_id}"));
            }
            if seen.insert(capability_id.to_string()) {
                normalized.push(capability_id.to_string());
            }
        }
        normalized.sort();
        Ok(normalized)
    }

    pub fn compose_prompt(
        &self,
        role_id: &str,
        role_prompt: &str,
        capability_ids: &[String],
    ) -> Result<String, String> {
        let mut sections = vec![role_prompt.trim().to_string()];
        sections.push(format!(
            "--- 当前任务激活的专业能力 ---\n以下能力只补充专业方法、工具策略和验收标准，不改变你作为 {role_id} 的职责与权限。只执行当前任务需要的内容，不把能力清单扩写成额外任务。"
        ));
        for capability_id in capability_ids {
            let capability = self
                .get(capability_id)
                .ok_or_else(|| format!("专业能力不存在: {capability_id}"))?;
            sections.push(format!(
                "## {} (`{}` · v{})\n{}",
                capability.display_name,
                capability.id,
                capability.version,
                capability.system_prompt.trim()
            ));
        }
        Ok(sections.join("\n\n"))
    }
}

fn user_capability_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".magi").join("capabilities"))
}

fn load_dir(dir: &Path) -> Result<Vec<ProfessionalCapability>, String> {
    let entries = fs::read_dir(dir)
        .map_err(|error| format!("读取专业能力目录 {} 失败: {error}", dir.display()))?;
    let mut capabilities = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        match fs::read_to_string(&path)
            .map_err(|error| format!("读取 {} 失败: {error}", path.display()))
            .and_then(|raw| parse_capability_markdown(&raw))
        {
            Ok(capability) => capabilities.push(capability),
            Err(error) => tracing::warn!(%error, file = %path.display(), "跳过无效专业能力文件"),
        }
    }
    Ok(capabilities)
}

fn builtin_capabilities_map() -> HashMap<String, ProfessionalCapability> {
    BUILTIN_CAPABILITY_SOURCES
        .iter()
        .map(|(label, raw)| {
            let capability = parse_capability_markdown(raw).unwrap_or_else(|error| {
                panic!("内置专业能力 {label} 解析失败: {error}");
            });
            (capability.id.clone(), capability)
        })
        .collect()
}

const BUILTIN_CAPABILITY_SOURCES: &[(&str, &str)] = &[
    (
        "general_engineering",
        include_str!("../assets/builtin-capabilities/general-engineering.md"),
    ),
    (
        "product_design",
        include_str!("../assets/builtin-capabilities/product-design.md"),
    ),
    (
        "frontend",
        include_str!("../assets/builtin-capabilities/frontend.md"),
    ),
    (
        "backend",
        include_str!("../assets/builtin-capabilities/backend.md"),
    ),
    (
        "desktop",
        include_str!("../assets/builtin-capabilities/desktop.md"),
    ),
    (
        "mobile",
        include_str!("../assets/builtin-capabilities/mobile.md"),
    ),
    (
        "database",
        include_str!("../assets/builtin-capabilities/database.md"),
    ),
    (
        "security",
        include_str!("../assets/builtin-capabilities/security.md"),
    ),
    (
        "devops",
        include_str!("../assets/builtin-capabilities/devops.md"),
    ),
    (
        "data_engineering",
        include_str!("../assets/builtin-capabilities/data-engineering.md"),
    ),
    (
        "ai_model_integration",
        include_str!("../assets/builtin-capabilities/ai-model-integration.md"),
    ),
    (
        "quality_engineering",
        include_str!("../assets/builtin-capabilities/quality-engineering.md"),
    ),
    (
        "performance",
        include_str!("../assets/builtin-capabilities/performance.md"),
    ),
];

fn parse_capability_markdown(raw: &str) -> Result<ProfessionalCapability, String> {
    let trimmed = raw.trim_start_matches('\u{feff}').trim_start();
    let after_open = trimmed
        .strip_prefix("---\n")
        .or_else(|| trimmed.strip_prefix("---\r\n"))
        .ok_or_else(|| "缺少起始 `---` 行".to_string())?;
    let close =
        super::find_close_delimiter(after_open).ok_or_else(|| "缺少结束 `---` 行".to_string())?;
    let header = &after_open[..close.start];
    let body = after_open[close.end..].trim_start_matches(['\n', '\r']);

    let mut id = String::new();
    let mut display_name = String::new();
    let mut description = String::new();
    let mut supported_roles = Vec::new();
    let mut version = 1;

    for (index, line) in header.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("第 {} 行不是 key: value 形式", index + 1))?;
        let value = value.trim();
        match key.trim() {
            "id" => id = super::strip_inline_quotes(value).to_string(),
            "display_name" => display_name = super::strip_inline_quotes(value).to_string(),
            "description" => description = super::strip_inline_quotes(value).to_string(),
            "supported_roles" => {
                supported_roles = parse_string_list(value, index + 1, "supported_roles")?
            }
            "version" => {
                version = value
                    .parse()
                    .map_err(|error| format!("第 {} 行 version 不是整数: {error}", index + 1))?
            }
            unknown => return Err(format!("第 {} 行未识别字段 `{unknown}`", index + 1)),
        }
    }

    if id.trim().is_empty() {
        return Err("缺少专业能力 id".to_string());
    }
    if display_name.trim().is_empty() {
        return Err(format!("专业能力 {id} 缺少 display_name"));
    }
    if description.trim().is_empty() {
        return Err(format!("专业能力 {id} 缺少 description"));
    }
    if supported_roles.is_empty() {
        return Err(format!("专业能力 {id} 缺少 supported_roles"));
    }
    let system_prompt = body.trim().to_string();
    if system_prompt.is_empty() {
        return Err(format!("专业能力 {id} 缺少提示词正文"));
    }

    Ok(ProfessionalCapability {
        id,
        display_name,
        description,
        supported_roles,
        system_prompt,
        version,
    })
}

fn parse_string_list(value: &str, line: usize, field: &str) -> Result<Vec<String>, String> {
    let inner = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or_else(|| format!("第 {line} 行 {field} 期望 [a, b] 格式"))?;
    let mut values = Vec::new();
    for item in inner.split(',') {
        let item = super::strip_inline_quotes(item.trim());
        if !item.is_empty() && !values.iter().any(|value| value == item) {
            values.push(item.to_string());
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_capabilities_are_versioned_and_role_scoped() {
        let registry = ProfessionalCapabilityRegistry::builtin();
        let frontend = registry
            .get("frontend")
            .expect("missing frontend capability");
        assert_eq!(frontend.version, 1);
        assert!(frontend.supports_role("executor"));
        assert!(!frontend.supports_role("coordinator"));
        assert_eq!(registry.ids().len(), BUILTIN_CAPABILITY_SOURCES.len());
    }

    #[test]
    fn validation_rejects_missing_or_unowned_capabilities() {
        let registry = ProfessionalCapabilityRegistry::builtin();
        assert!(registry.validate_ids_for_role("executor", &[]).is_err());
        assert!(
            registry
                .validate_ids_for_role("coordinator", &["frontend".to_string()])
                .is_err()
        );
    }

    #[test]
    fn composed_prompt_is_deterministic_and_keeps_role_boundary() {
        let registry = ProfessionalCapabilityRegistry::builtin();
        let ids = registry
            .validate_ids_for_role(
                "executor",
                &["security".to_string(), "frontend".to_string()],
            )
            .expect("capabilities should validate");
        let prompt = registry
            .compose_prompt("executor", "你是执行工程师。", &ids)
            .expect("prompt should compose");
        assert!(prompt.starts_with("你是执行工程师。"));
        assert!(prompt.contains("`frontend`"));
        assert!(prompt.contains("`security`"));
        assert!(prompt.find("`frontend`") < prompt.find("`security`"));
    }
}
