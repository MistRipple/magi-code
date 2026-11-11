//! 对话式创建与修改用户自定义角色：`agent_role_get` 读取、`agent_role_save` 保存。
//!
//! 保存走 `AgentRoleRegistry::save_user_role`，与设置界面是同一条写入路径，校验、版本冲突
//! 和内置角色保护都由注册表裁决，这里不另存状态。保存前由调用方先取得用户确认
//! （`prepare_agent_role_save` 只做检查与摘要，不写入）。

use magi_agent_role::{
    AgentRole, AgentRoleError, AgentRoleRegistry, CURRENT_ROLE_SCHEMA_VERSION, TaskKindLabel,
};
use magi_core::ExecutionResultStatus;
use magi_tool_runtime::BuiltinToolName;
use serde_json::{Value, json};

type ToolResult = (String, ExecutionResultStatus);

const SAVE_FIELDS: &[&str] = &[
    "role_id",
    "display_name",
    "description",
    "system_prompt",
    "role",
    "focus",
    "constraints",
    "output_preferences",
    "ownerships",
    "insight_preferences",
    "capabilities",
    "parallelism_limit",
    "color_token",
    "icon",
    "expected_role_revision",
];

/// 已通过检查、等待用户确认的保存请求。
pub(crate) struct RoleSaveProposal {
    role: AgentRole,
    expected_role_revision: Option<u64>,
    updated: bool,
    summary: String,
}

impl RoleSaveProposal {
    /// 展示给用户确认的变更摘要。
    pub(crate) fn summary(&self) -> &str {
        &self.summary
    }
}

pub(crate) enum RoleSavePreparation {
    /// 需要用户确认后才能保存。
    Confirm(Box<RoleSaveProposal>),
    /// 与现有内容完全一致，不需要保存。
    Unchanged(ToolResult),
}

fn failure(tool: BuiltinToolName, code: &str, error: impl ToString) -> ToolResult {
    (
        json!({
            "tool": tool.as_str(),
            "status": "failed",
            "error_code": code,
            "error": error.to_string(),
        })
        .to_string(),
        ExecutionResultStatus::Failed,
    )
}

fn role_error(tool: BuiltinToolName, error: AgentRoleError) -> ToolResult {
    match error {
        AgentRoleError::Conflict(message) => failure(tool, "role_conflict", message),
        AgentRoleError::InvalidDefinition(message) | AgentRoleError::Parse { message, .. } => {
            failure(tool, "role_invalid", message)
        }
        AgentRoleError::InvalidId { file_stem, .. } => {
            failure(tool, "role_invalid", format!("角色 id 无效：{file_stem}"))
        }
        AgentRoleError::Io { .. } => failure(tool, "role_persist_failed", "角色文件保存失败"),
    }
}

fn role_json(registry: &AgentRoleRegistry, role: &AgentRole, with_prompt: bool) -> Value {
    let mut value = json!({
        "role_id": role.id,
        "display_name": role.display_name,
        "description": role.description,
        "source": if registry.is_builtin(&role.id) { "builtin" } else { "user" },
        "editable": !registry.is_builtin(&role.id),
        "role_revision": role.role_revision,
        "read_only": role.is_read_only(),
        "capabilities": role.capabilities,
    });
    if with_prompt {
        value["system_prompt"] = json!(role.system_prompt);
        value["role"] = json!(role.role);
        value["focus"] = json!(role.focus);
        value["constraints"] = json!(role.constraints);
        value["output_preferences"] = json!(role.output_preferences);
        value["ownerships"] = json!(role.ownerships);
        value["insight_preferences"] = json!(role.insight_preferences);
        value["parallelism_limit"] = json!(role.parallelism_limit);
        value["color_token"] = json!(role.color_token);
        value["icon"] = json!(role.icon);
    }
    value
}

/// `agent_role_get`：不带 `role_id` 列出全部角色与可选专业能力，带则返回该角色完整定义。
pub(crate) fn execute_agent_role_get(registry: &AgentRoleRegistry, arguments: &str) -> ToolResult {
    let tool = BuiltinToolName::AgentRoleGet;
    let parsed: Value = match serde_json::from_str(if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments
    }) {
        Ok(value) => value,
        Err(error) => {
            return failure(
                tool,
                "invalid_arguments",
                format!("参数不是有效 JSON：{error}"),
            );
        }
    };
    let Some(object) = parsed.as_object() else {
        return failure(tool, "invalid_arguments", "参数必须是 JSON 对象");
    };
    if let Some(key) = object.keys().find(|key| key.as_str() != "role_id") {
        return failure(
            tool,
            "invalid_arguments",
            format!("agent_role_get 不支持字段 {key}"),
        );
    }
    let payload = match object.get("role_id") {
        None | Some(Value::Null) => json!({
            "tool": tool.as_str(),
            "status": "ok",
            "roles": registry.all().iter()
                .filter(|role| !role.coordinator_mode)
                .map(|role| role_json(registry, role, false))
                .collect::<Vec<_>>(),
            "available_capabilities": registry.capability_summaries().into_iter()
                .map(|capability| json!({
                    "id": capability.id,
                    "display_name": capability.display_name,
                    "description": capability.description,
                }))
                .collect::<Vec<_>>(),
        }),
        Some(Value::String(role_id)) => {
            let Some(role) = registry.get(role_id.trim()) else {
                return failure(tool, "role_not_found", format!("角色 {role_id} 不存在"));
            };
            let mut role_value = role_json(registry, &role, true);
            if registry.is_builtin(&role.id) {
                role_value["note"] =
                    json!("内置角色不能修改；需要调整时以它为基础另存为新的自定义角色。");
            }
            json!({"tool": tool.as_str(), "status": "ok", "role": role_value})
        }
        Some(_) => return failure(tool, "invalid_arguments", "role_id 必须是字符串"),
    };
    (payload.to_string(), ExecutionResultStatus::Succeeded)
}

fn optional_text(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.trim().to_string())),
        Some(_) => Err(format!("{key} 必须是字符串")),
    }
}

fn optional_list(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<String>>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(|value| value.trim().to_string())
                    .ok_or_else(|| format!("{key} 只能包含字符串"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(format!("{key} 必须是字符串数组")),
    }
}

fn optional_uint(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<u64>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{key} 必须是正整数")),
    }
}

/// 把工具参数转成角色定义。修改已有角色时未提供的可选字段沿用现有值。
fn role_from_arguments(
    registry: &AgentRoleRegistry,
    object: &serde_json::Map<String, Value>,
    expected_role_revision: Option<u64>,
) -> Result<AgentRole, String> {
    let role_id = optional_text(object, "role_id")?.unwrap_or_default();
    let base = expected_role_revision.and_then(|_| registry.get(&role_id));
    let text = |key: &str, current: Option<&str>| -> Result<String, String> {
        Ok(optional_text(object, key)?
            .or_else(|| current.map(str::to_string))
            .unwrap_or_default())
    };
    let list = |key: &str, current: Option<&Vec<String>>| -> Result<Vec<String>, String> {
        Ok(optional_list(object, key)?
            .or_else(|| current.cloned())
            .unwrap_or_default())
    };
    let parallelism_limit = match optional_uint(object, "parallelism_limit")? {
        Some(value) => {
            Some(u32::try_from(value).map_err(|_| "parallelism_limit 超出范围".to_string())?)
        }
        None => base.as_ref().and_then(|role| role.parallelism_limit),
    };
    Ok(AgentRole {
        id: role_id,
        system_prompt: text(
            "system_prompt",
            base.as_ref().map(|role| role.system_prompt.as_str()),
        )?,
        display_name: text(
            "display_name",
            base.as_ref().map(|role| role.display_name.as_str()),
        )?,
        description: text(
            "description",
            base.as_ref().map(|role| role.description.as_str()),
        )?,
        supported_kinds: vec![TaskKindLabel::LocalAgent],
        parallelism_limit,
        coordinator_mode: false,
        version: CURRENT_ROLE_SCHEMA_VERSION,
        role_revision: 1,
        role: text("role", base.as_ref().map(|role| role.role.as_str()))?,
        focus: list("focus", base.as_ref().map(|role| &role.focus))?,
        constraints: list("constraints", base.as_ref().map(|role| &role.constraints))?,
        output_preferences: list(
            "output_preferences",
            base.as_ref().map(|role| &role.output_preferences),
        )?,
        ownerships: list("ownerships", base.as_ref().map(|role| &role.ownerships))?,
        insight_preferences: list(
            "insight_preferences",
            base.as_ref().map(|role| &role.insight_preferences),
        )?,
        capabilities: list("capabilities", base.as_ref().map(|role| &role.capabilities))?,
        color_token: text(
            "color_token",
            base.as_ref().map(|role| role.color_token.as_str()),
        )?,
        icon: text("icon", base.as_ref().map(|role| role.icon.as_str()))?,
    })
}

fn list_text(values: &[String]) -> String {
    if values.is_empty() {
        "无".to_string()
    } else {
        values.join("、")
    }
}

/// 专业能力在确认说明里用显示名，不暴露内部 id；注册表里没有说明的能力回退到 id。
fn capability_names(registry: &AgentRoleRegistry, ids: &[String]) -> Vec<String> {
    let summaries = registry.capability_summaries();
    ids.iter()
        .map(|id| {
            summaries
                .iter()
                .find(|summary| &summary.id == id)
                .map(|summary| summary.display_name.clone())
                .unwrap_or_else(|| id.clone())
        })
        .collect()
}

/// 列表字段的变更只说新增和移除了哪些项：真实角色的约束常是整句，整表前后对照既长又难读。
fn list_change(label: &str, before: &[String], after: &[String]) -> Option<String> {
    if before == after {
        return None;
    }
    let added = after
        .iter()
        .filter(|item| !before.contains(item))
        .cloned()
        .collect::<Vec<_>>();
    let removed = before
        .iter()
        .filter(|item| !after.contains(item))
        .cloned()
        .collect::<Vec<_>>();
    let mut parts = Vec::new();
    if !added.is_empty() {
        parts.push(format!("新增 {}", added.join("；")));
    }
    if !removed.is_empty() {
        parts.push(format!("移除 {}", removed.join("；")));
    }
    if parts.is_empty() {
        parts.push("顺序已调整".to_string());
    }
    Some(format!("{label}：{}", parts.join("；")))
}

/// 新建的确认说明：按行展示，只列出有内容的可选项。
fn created_summary(registry: &AgentRoleRegistry, role: &AgentRole) -> String {
    let mut lines = vec![
        format!("创建自定义角色「{}」（{}）", role.display_name, role.id),
        format!(
            "说明：{}",
            if role.description.is_empty() {
                "无"
            } else {
                &role.description
            }
        ),
        format!(
            "专业能力：{}",
            list_text(&capability_names(registry, &role.capabilities))
        ),
    ];
    if !role.constraints.is_empty() {
        lines.push(format!("约束：{}", list_text(&role.constraints)));
    }
    lines.push(format!(
        "系统提示词：{} 字",
        role.system_prompt.chars().count()
    ));
    lines.join("\n")
}

fn changed_fields(
    registry: &AgentRoleRegistry,
    before: &AgentRole,
    after: &AgentRole,
) -> Vec<String> {
    let mut changes = Vec::new();
    if before.display_name != after.display_name {
        changes.push(format!(
            "显示名称：{} → {}",
            before.display_name, after.display_name
        ));
    }
    if before.description != after.description {
        changes.push(format!(
            "说明：{} → {}",
            before.description, after.description
        ));
    }
    if before.system_prompt != after.system_prompt {
        changes.push(format!(
            "系统提示词已修改（{} 字 → {} 字）",
            before.system_prompt.chars().count(),
            after.system_prompt.chars().count()
        ));
    }
    if before.role != after.role {
        changes.push(format!("定位：{} → {}", before.role, after.role));
    }
    let lists = [
        ("关注点", &before.focus, &after.focus),
        ("约束", &before.constraints, &after.constraints),
        (
            "输出偏好",
            &before.output_preferences,
            &after.output_preferences,
        ),
        ("负责领域", &before.ownerships, &after.ownerships),
        (
            "洞察类型",
            &before.insight_preferences,
            &after.insight_preferences,
        ),
    ];
    for (label, old, new) in lists {
        if let Some(change) = list_change(label, old, new) {
            changes.push(change);
        }
    }
    if let Some(change) = list_change(
        "专业能力",
        &capability_names(registry, &before.capabilities),
        &capability_names(registry, &after.capabilities),
    ) {
        changes.push(change);
    }
    if before.parallelism_limit != after.parallelism_limit {
        let show =
            |value: Option<u32>| value.map_or("不限制".to_string(), |limit| limit.to_string());
        changes.push(format!(
            "实例上限：{} → {}",
            show(before.parallelism_limit),
            show(after.parallelism_limit)
        ));
    }
    if before.color_token != after.color_token {
        changes.push("颜色标记已修改".to_string());
    }
    if before.icon != after.icon {
        changes.push("图标已修改".to_string());
    }
    changes
}

/// `agent_role_save` 的检查阶段：解析参数、套用保存时同一套校验并生成给用户确认的摘要，不写入。
pub(crate) fn prepare_agent_role_save(
    registry: &AgentRoleRegistry,
    arguments: &str,
) -> Result<RoleSavePreparation, ToolResult> {
    let tool = BuiltinToolName::AgentRoleSave;
    let parsed: Value = serde_json::from_str(arguments).map_err(|error| {
        failure(
            tool,
            "invalid_arguments",
            format!("参数不是有效 JSON：{error}"),
        )
    })?;
    let object = parsed
        .as_object()
        .ok_or_else(|| failure(tool, "invalid_arguments", "参数必须是 JSON 对象"))?;
    if let Some(key) = object
        .keys()
        .find(|key| !SAVE_FIELDS.contains(&key.as_str()))
    {
        return Err(failure(
            tool,
            "invalid_arguments",
            format!("agent_role_save 不支持字段 {key}"),
        ));
    }
    let expected_role_revision = optional_uint(object, "expected_role_revision")
        .map_err(|error| failure(tool, "invalid_arguments", error))?;
    let role = role_from_arguments(registry, object, expected_role_revision)
        .map_err(|error| failure(tool, "invalid_arguments", error))?;
    let existing = registry.get(&role.id);
    // 检查阶段就套用保存时的全部规则，避免请用户确认一份根本保存不了的角色。
    let checked = registry
        .check_user_role(role.clone(), expected_role_revision)
        .map_err(|error| role_error(tool, error))?;
    let (updated, summary) = match existing {
        Some(existing) => {
            let changes = changed_fields(registry, &existing, &checked);
            if changes.is_empty() {
                return Ok(RoleSavePreparation::Unchanged((
                    json!({
                        "tool": tool.as_str(),
                        "status": "ok",
                        "mode": "unchanged",
                        "role_id": existing.id,
                        "role_revision": existing.role_revision,
                        "message": "角色内容与现有版本一致，没有需要保存的变更。",
                    })
                    .to_string(),
                    ExecutionResultStatus::Succeeded,
                )));
            }
            (
                true,
                format!(
                    "修改自定义角色「{}」（{}）\n{}",
                    checked.display_name,
                    checked.id,
                    changes
                        .iter()
                        .map(|change| format!("· {change}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            )
        }
        None => (false, created_summary(registry, &checked)),
    };
    Ok(RoleSavePreparation::Confirm(Box::new(RoleSaveProposal {
        role,
        expected_role_revision,
        updated,
        summary,
    })))
}

/// 用户确认后保存。版本冲突等由注册表在保存时再次裁决。
pub(crate) fn commit_agent_role_save(
    registry: &AgentRoleRegistry,
    proposal: Box<RoleSaveProposal>,
) -> (ToolResult, Option<String>) {
    let tool = BuiltinToolName::AgentRoleSave;
    let proposal = *proposal;
    match registry.save_user_role(proposal.role, proposal.expected_role_revision) {
        Ok(saved) => {
            let payload = json!({
                "tool": tool.as_str(),
                "status": "ok",
                "mode": if proposal.updated { "updated" } else { "created" },
                "role": role_json(registry, &saved, true),
                "role_revision": saved.role_revision,
                "next": "角色已保存，之后派发即可选用。提醒用户可在 设置 → 代理 中为它绑定模型，未绑定时继承主对话模型。",
            });
            (
                (payload.to_string(), ExecutionResultStatus::Succeeded),
                Some(saved.id),
            )
        }
        Err(error) => (role_error(tool, error), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> (AgentRoleRegistry, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (
            AgentRoleRegistry::builtin().with_user_role_dir(dir.path().join("roles")),
            dir,
        )
    }

    fn create_args(id: &str) -> String {
        json!({
            "role_id": id,
            "display_name": "数据分析师",
            "description": "分析数据并给出结论",
            "system_prompt": "你负责分析给定数据，输出结论与证据。",
            "constraints": ["read-only"],
            "capabilities": ["general_engineering"],
        })
        .to_string()
    }

    fn prepared(registry: &AgentRoleRegistry, arguments: &str) -> Box<RoleSaveProposal> {
        match prepare_agent_role_save(registry, arguments) {
            Ok(RoleSavePreparation::Confirm(proposal)) => proposal,
            Ok(RoleSavePreparation::Unchanged(result)) => panic!("意外的无变更：{}", result.0),
            Err(result) => panic!("检查失败：{}", result.0),
        }
    }

    fn error_code(result: &ToolResult) -> String {
        serde_json::from_str::<Value>(&result.0).unwrap()["error_code"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn get_lists_roles_and_capabilities_without_exposing_the_coordinator() {
        let (registry, _dir) = registry();
        let (payload, status) = execute_agent_role_get(&registry, "{}");
        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let value: Value = serde_json::from_str(&payload).unwrap();
        let ids = value["roles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|role| role["role_id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"executor"));
        assert!(!ids.contains(&"coordinator"));
        assert!(
            value["available_capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|capability| capability["id"] == "general_engineering")
        );

        let (payload, _) = execute_agent_role_get(&registry, r#"{"role_id":"reviewer"}"#);
        let value: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value["role"]["editable"], false);
        assert!(value["role"]["system_prompt"].as_str().unwrap().len() > 10);
        assert!(value["role"]["note"].as_str().unwrap().contains("另存"));
    }

    #[test]
    fn create_requires_confirmation_summary_then_persists_through_the_registry() {
        let (registry, _dir) = registry();
        let proposal = prepared(&registry, &create_args("data-analyst"));
        assert!(proposal.summary().contains("创建自定义角色「数据分析师」"));
        assert!(registry.get("data-analyst").is_none(), "确认前不能写入");
        let ((payload, status), saved) = commit_agent_role_save(&registry, proposal);
        assert_eq!(status, ExecutionResultStatus::Succeeded);
        assert_eq!(saved.as_deref(), Some("data-analyst"));
        let value: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value["mode"], "created");
        assert_eq!(value["role_revision"], 1);
        let role = registry.get("data-analyst").unwrap();
        assert!(role.is_read_only() && !role.coordinator_mode);
        assert_eq!(role.supported_kinds, vec![TaskKindLabel::LocalAgent]);
    }

    #[test]
    fn update_overlays_omitted_fields_and_reports_only_what_changed() {
        let (registry, _dir) = registry();
        commit_agent_role_save(&registry, prepared(&registry, &create_args("data-analyst")));
        let arguments = json!({
            "role_id": "data-analyst",
            "display_name": "数据分析师",
            "system_prompt": "你负责分析给定数据，先说明口径再给结论。",
            "capabilities": ["general_engineering"],
            "expected_role_revision": 1,
        })
        .to_string();
        let proposal = prepared(&registry, &arguments);
        let summary = proposal.summary().to_string();
        assert!(summary.contains("修改自定义角色") && summary.contains("系统提示词已修改"));
        assert!(
            !summary.contains("约束"),
            "未改动的字段不进入摘要: {summary}"
        );
        let ((payload, _), _) = commit_agent_role_save(&registry, proposal);
        assert_eq!(
            serde_json::from_str::<Value>(&payload).unwrap()["mode"],
            "updated"
        );
        let role = registry.get("data-analyst").unwrap();
        assert_eq!(role.role_revision, 2);
        assert_eq!(
            role.constraints,
            vec!["read-only".to_string()],
            "省略的字段沿用现有值"
        );
        assert_eq!(role.description, "分析数据并给出结论");
    }

    #[test]
    fn list_changes_name_only_the_added_and_removed_items() {
        let (registry, _dir) = registry();
        let created = json!({
            "role_id": "data-analyst", "display_name": "数据分析师", "system_prompt": "你负责分析数据。",
            "constraints": ["read-only", "不修改任何文件，只做读取与分析"],
            "capabilities": ["general_engineering"],
        })
        .to_string();
        commit_agent_role_save(&registry, prepared(&registry, &created));
        let arguments = json!({
            "role_id": "data-analyst", "display_name": "数据分析师", "system_prompt": "你负责分析数据。",
            "constraints": ["read-only", "不得联网"],
            "capabilities": ["general_engineering", "database"],
            "expected_role_revision": 1,
        })
        .to_string();
        let summary = prepared(&registry, &arguments).summary().to_string();
        assert!(
            summary.contains("约束：新增 不得联网；移除 不修改任何文件，只做读取与分析"),
            "{summary}"
        );
        assert!(summary.contains("专业能力：新增"), "{summary}");
        assert!(
            !summary.contains("read-only"),
            "未变化的项不进入摘要: {summary}"
        );
        assert!(!summary.contains("系统提示词"), "{summary}");
    }

    #[test]
    fn identical_content_is_reported_as_unchanged_without_a_confirmation() {
        let (registry, _dir) = registry();
        commit_agent_role_save(&registry, prepared(&registry, &create_args("data-analyst")));
        let arguments = json!({
            "role_id": "data-analyst", "display_name": "数据分析师",
            "system_prompt": "你负责分析给定数据，输出结论与证据。",
            "capabilities": ["general_engineering"], "expected_role_revision": 1,
        })
        .to_string();
        match prepare_agent_role_save(&registry, &arguments) {
            Ok(RoleSavePreparation::Unchanged((payload, _))) => {
                assert!(payload.contains("unchanged"));
            }
            _ => panic!("应识别为无变更"),
        }
    }

    #[test]
    fn registry_rules_are_applied_before_asking_the_user() {
        let (registry, _dir) = registry();
        // 内置角色不能被覆盖，已存在的用户角色必须带版本号，未知能力被拒绝。
        let builtin = prepare_agent_role_save(&registry, &create_args("executor"));
        assert_eq!(error_code(&builtin.err().unwrap()), "role_conflict");
        commit_agent_role_save(&registry, prepared(&registry, &create_args("data-analyst")));
        let duplicate = prepare_agent_role_save(&registry, &create_args("data-analyst"));
        assert_eq!(error_code(&duplicate.err().unwrap()), "role_conflict");
        let stale = json!({
            "role_id": "data-analyst", "display_name": "数据分析师", "system_prompt": "x",
            "capabilities": ["general_engineering"], "expected_role_revision": 9,
        })
        .to_string();
        assert_eq!(
            error_code(&prepare_agent_role_save(&registry, &stale).err().unwrap()),
            "role_conflict"
        );
        let unknown =
            create_args("other-role").replace("general_engineering", "no-such-capability");
        assert_eq!(
            error_code(&prepare_agent_role_save(&registry, &unknown).err().unwrap()),
            "role_invalid"
        );
        let missing = json!({"role_id": "ghost", "display_name": "x", "system_prompt": "x",
            "capabilities": ["general_engineering"], "expected_role_revision": 1})
        .to_string();
        assert_eq!(
            error_code(&prepare_agent_role_save(&registry, &missing).err().unwrap()),
            "role_conflict"
        );
    }

    #[test]
    fn arguments_cannot_set_coordinator_mode_or_unknown_fields() {
        let (registry, _dir) = registry();
        let args = json!({
            "role_id": "sneaky", "display_name": "x", "system_prompt": "x",
            "capabilities": ["general_engineering"], "coordinator_mode": true,
        })
        .to_string();
        assert_eq!(
            error_code(&prepare_agent_role_save(&registry, &args).err().unwrap()),
            "invalid_arguments"
        );
        assert_eq!(
            error_code(&prepare_agent_role_save(&registry, "[]").err().unwrap()),
            "invalid_arguments"
        );
    }
}
