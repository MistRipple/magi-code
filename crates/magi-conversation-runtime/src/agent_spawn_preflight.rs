//! `agent_spawn` 的唯一创建前预检入口。
//!
//! 预检只读取输入、角色注册表、当前执行链、计划、模型和工作区事实，不写入
//! TaskStore、SessionStore、SpawnGraph、执行注册表或准入队列。只有预检成功后，
//! `tool_batch` 才会生成 child task id 并调用原子注册入口。

use std::{collections::HashSet, path::PathBuf, sync::Arc};

use magi_bridge_client::ModelBridgeClient;
use magi_core::{
    AccessProfile, AgentContextPackage, AgentContextReference, AgentContextReferenceKind,
    PlanItemStatus, Task, TaskId, TaskKind, TaskPolicy, TaskStatus, TaskTier, UtcMillis,
    WorkspaceId, estimate_text_tokens,
};
use magi_orchestrator::task_store::TaskStore;
use magi_plan::PlanStore;
use magi_session_store::SessionStore;
use magi_settings_store::SettingsStore;

use crate::{
    model_config::configured_role_engine_model_config,
    task_execution_dispatcher::{RoleTarget, resolve_target_for_role},
    task_execution_registry::{TaskExecutionPlan, TaskExecutionRegistry},
    task_helpers::task_is_coordinator,
};

pub(crate) const AGENT_CONTEXT_SUMMARY_MAX_CHARS: usize = 4_000;
pub(crate) const AGENT_CONTEXT_EXPECTED_OUTPUT_MAX_CHARS: usize = 2_000;
pub(crate) const AGENT_CONTEXT_CONSTRAINT_MAX_CHARS: usize = 600;
pub(crate) const AGENT_CONTEXT_PREVIEW_MAX_CHARS: usize = 600;
pub(crate) const AGENT_CONTEXT_REFERENCE_LIMIT: usize = 16;
pub(crate) const AGENT_SPAWN_GOAL_MAX_CHARS: usize = 20_000;

const AGENT_SPAWN_FIELDS: &[&str] = &[
    "task_name",
    "plan_item_id",
    "role",
    "capabilities",
    "display_name",
    "goal",
    "task_kind",
    "context_package",
    "context",
    "working_dir",
    "parallelism_group",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentSpawnPreflightError {
    pub(crate) error_code: String,
    pub(crate) failure_stage: String,
    pub(crate) message: String,
    pub(crate) instruction: String,
}

impl AgentSpawnPreflightError {
    fn input(
        error_code: impl Into<String>,
        message: impl Into<String>,
        instruction: impl Into<String>,
    ) -> Self {
        Self {
            error_code: error_code.into(),
            failure_stage: "input_validation".to_string(),
            message: message.into(),
            instruction: instruction.into(),
        }
    }

    fn runtime(
        error_code: impl Into<String>,
        stage: impl Into<String>,
        message: impl Into<String>,
        instruction: impl Into<String>,
    ) -> Self {
        Self {
            error_code: error_code.into(),
            failure_stage: stage.into(),
            message: message.into(),
            instruction: instruction.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AgentSpawnPreflight {
    pub(crate) task_name: String,
    pub(crate) canonical_task_name: String,
    pub(crate) role: String,
    pub(crate) capability_ids: Vec<String>,
    pub(crate) display_name: String,
    pub(crate) goal: String,
    pub(crate) task_kind: TaskKind,
    pub(crate) context_package: AgentContextPackage,
    pub(crate) plan_item_id: Option<magi_core::PlanItemId>,
    pub(crate) parallelism_group: Option<String>,
    pub(crate) working_dir: Option<PathBuf>,
    pub(crate) child_policy_snapshot: TaskPolicy,
    pub(crate) child_access_profile: AccessProfile,
    pub(crate) child_dependency_ids: Vec<TaskId>,
    pub(crate) child_input_refs: Vec<String>,
    pub(crate) queue_reason: Option<String>,
    pub(crate) model_source: Option<String>,
}

pub(crate) struct AgentSpawnPreflightInput<'a> {
    pub(crate) parsed: &'a serde_json::Value,
    pub(crate) parent_task: &'a Task,
    pub(crate) task_store: &'a TaskStore,
    pub(crate) session_store: &'a SessionStore,
    pub(crate) execution_registry: &'a TaskExecutionRegistry,
    pub(crate) agent_role_registry: &'a magi_agent_role::AgentRoleRegistry,
    pub(crate) plan_store: &'a PlanStore,
    pub(crate) session_id: &'a magi_core::SessionId,
    pub(crate) workspace_id: &'a Option<WorkspaceId>,
    pub(crate) now: UtcMillis,
    pub(crate) sequence: u64,
}

pub(crate) fn preflight_agent_spawn(
    input: AgentSpawnPreflightInput<'_>,
) -> Result<AgentSpawnPreflight, AgentSpawnPreflightError> {
    let AgentSpawnPreflightInput {
        parsed,
        parent_task,
        task_store,
        session_store,
        execution_registry,
        agent_role_registry,
        plan_store,
        session_id,
        workspace_id,
        now,
        sequence,
    } = input;

    let Some(object) = parsed.as_object() else {
        return Err(AgentSpawnPreflightError::input(
            "invalid_arguments",
            "agent_spawn 参数必须是 JSON 对象",
            "请使用 agent_spawn 的完整 JSON Schema 重新提交对象参数。",
        ));
    };
    if let Some(unknown) = object
        .keys()
        .find(|key| !AGENT_SPAWN_FIELDS.contains(&key.as_str()))
    {
        return Err(AgentSpawnPreflightError::input(
            "invalid_arguments",
            format!("agent_spawn 不支持字段 {unknown}"),
            "请移除未知字段，并按当前 agent_spawn Schema 重新提交。",
        ));
    }
    if object.contains_key("context") {
        return Err(AgentSpawnPreflightError::input(
            "legacy_context_rejected",
            "agent_spawn 不再接受 context 字符串，请使用结构化 context_package",
            "请移除 context，并按 Schema 传入 context_package 对象。",
        ));
    }

    let task_name = required_string(object, "task_name")?;
    if !valid_agent_task_name(&task_name) {
        return Err(AgentSpawnPreflightError::input(
            "invalid_task_name",
            "agent_spawn task_name 只允许小写字母、数字和下划线，长度必须为 1-48 个字符",
            "请生成一个唯一的小写 task_name 后重新调用 agent_spawn。",
        ));
    }
    let parent_canonical_name = parent_task.canonical_task_name().unwrap_or("/root");
    let canonical_task_name = child_canonical_task_name(parent_canonical_name, &task_name);
    if task_store
        .get_children(&parent_task.task_id)
        .iter()
        .any(|child| child.canonical_task_name() == Some(canonical_task_name.as_str()))
    {
        return Err(AgentSpawnPreflightError::input(
            "duplicate_task_name",
            format!("同一父任务下 task_name 已存在: {task_name}"),
            "请生成一个未使用的 task_name 后重新调用 agent_spawn。",
        ));
    }

    let role = required_string(object, "role")?;
    let Some(role_definition) = agent_role_registry.get(&role) else {
        return Err(AgentSpawnPreflightError::input(
            "agent_role_not_spawnable",
            format!("代理角色不存在: {role}"),
            "请先使用 tool_catalog 查询可派发角色，再选择有效 role。",
        ));
    };
    if role_definition.coordinator_mode
        || !agent_role_registry.is_spawnable_agent_role(&role)
        || !role_definition
            .supported_task_kinds()
            .contains(&TaskKind::LocalAgent)
    {
        return Err(AgentSpawnPreflightError::input(
            "agent_role_not_spawnable",
            format!("角色 {role} 不能作为 LocalAgent 派发目标"),
            "请从 tool_catalog 返回的 spawnable 角色中重新选择。",
        ));
    }

    let capability_ids = parse_capabilities(object, agent_role_registry, &role)?;
    let display_name = required_string(object, "display_name")?;
    let display_name_chars = display_name.chars().count();
    if !(3..=30).contains(&display_name_chars) {
        return Err(AgentSpawnPreflightError::input(
            "invalid_display_name",
            format!(
                "agent_spawn display_name 长度必须在 3-30 个字符之间，实际 {display_name_chars}"
            ),
            "请提供长度为 3-30 个字符的 display_name。",
        ));
    }
    let goal = required_string(object, "goal")?;
    if goal.chars().count() > AGENT_SPAWN_GOAL_MAX_CHARS {
        return Err(AgentSpawnPreflightError::input(
            "invalid_arguments",
            format!("agent_spawn goal 最多允许 {AGENT_SPAWN_GOAL_MAX_CHARS} 个字符"),
            "请压缩目标描述后重新提交。",
        ));
    }

    let task_kind = parse_task_kind(object)?;
    let plan_item_id =
        optional_nonempty_string(object, "plan_item_id")?.map(magi_core::PlanItemId::new);
    if let Some(item_id) = plan_item_id.as_ref() {
        let Some(plan) = plan_store.snapshot() else {
            return Err(AgentSpawnPreflightError::input(
                "plan_item_not_found",
                format!("agent_spawn plan_item_id 不存在: {item_id}"),
                "请使用当前 update_plan 返回的顶层 itemId，或省略 plan_item_id。",
            ));
        };
        let Some(item) = plan.items.iter().find(|item| &item.item_id == item_id) else {
            return Err(AgentSpawnPreflightError::input(
                "plan_item_not_found",
                format!("agent_spawn plan_item_id 不存在: {item_id}"),
                "请使用当前 update_plan 返回的顶层 itemId，或省略 plan_item_id。",
            ));
        };
        if item.status != PlanItemStatus::InProgress {
            return Err(AgentSpawnPreflightError::input(
                "plan_item_not_executable",
                format!(
                    "计划项 {item_id} 当前状态为 {}，不可绑定代理",
                    item.status.as_str()
                ),
                "请先将该计划项推进到 in_progress，或省略 plan_item_id。",
            ));
        }
    }

    let parallelism_group = optional_nonempty_string(object, "parallelism_group")?;
    let working_dir = parse_working_dir(object)?;
    let context_package = parse_agent_context_package(parsed, &parent_task.task_id, now, sequence)
        .map_err(|message| {
            AgentSpawnPreflightError::input(
                "invalid_context_package",
                message,
                "请按 agent_spawn Schema 传入结构化 context_package；summary、constraints、expected_output 和 references 必须是规定类型。",
            )
        })?;

    validate_execution_chain(
        parent_task,
        task_store,
        session_store,
        session_id,
        workspace_id,
        agent_role_registry,
    )?;
    let runtime = execution_registry.agent_spawn_preflight_runtime();
    let model_source = preflight_model(
        execution_registry,
        parent_task,
        &runtime.settings_store,
        runtime.default_model_client.clone(),
        &role,
        session_id,
    )?;
    preflight_workspace_and_git(
        execution_registry,
        parent_task,
        session_id,
        workspace_id,
        working_dir.as_ref(),
        &runtime,
    )?;

    let queue_reason = execution_registry.admission_preview(Some(session_id), &role);
    let child_policy_snapshot =
        agent_spawn_child_policy_snapshot(parent_task.policy_snapshot.as_ref());
    let child_access_profile = child_policy_snapshot.effective_access_profile();
    let child_dependency_ids = agent_spawn_child_dependency_ids(parent_task);
    let child_input_refs = context_package
        .references
        .iter()
        .map(|reference| reference.source_ref.clone())
        .collect();

    Ok(AgentSpawnPreflight {
        task_name,
        canonical_task_name,
        role,
        capability_ids,
        display_name,
        goal,
        task_kind,
        context_package,
        plan_item_id,
        parallelism_group,
        working_dir,
        child_policy_snapshot,
        child_access_profile,
        child_dependency_ids,
        child_input_refs,
        queue_reason,
        model_source,
    })
}

fn required_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String, AgentSpawnPreflightError> {
    let Some(value) = object.get(field) else {
        return Err(AgentSpawnPreflightError::input(
            "missing_required_fields",
            format!("agent_spawn 缺少必需字段 {field}"),
            format!("请补齐 {field}，并保持其为非空字符串。"),
        ));
    };
    let Some(value) = value.as_str() else {
        return Err(AgentSpawnPreflightError::input(
            "invalid_arguments",
            format!("agent_spawn {field} 必须是字符串"),
            format!("请将 {field} 改为非空 JSON string。"),
        ));
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(AgentSpawnPreflightError::input(
            "missing_required_fields",
            format!("agent_spawn {field} 不能为空"),
            format!("请补齐 {field}，并保持其为非空字符串。"),
        ));
    }
    Ok(value.to_string())
}

fn optional_nonempty_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<String>, AgentSpawnPreflightError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(value) = value.as_str() else {
        return Err(AgentSpawnPreflightError::input(
            "invalid_arguments",
            format!("agent_spawn {field} 必须是字符串或 null"),
            format!("请将 {field} 改为 JSON string，或省略该字段。"),
        ));
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(AgentSpawnPreflightError::input(
            "invalid_arguments",
            format!("agent_spawn {field} 不能为空字符串"),
            format!("请提供有效的 {field}，或省略该字段。"),
        ));
    }
    Ok(Some(value.to_string()))
}

fn parse_capabilities(
    object: &serde_json::Map<String, serde_json::Value>,
    registry: &magi_agent_role::AgentRoleRegistry,
    role: &str,
) -> Result<Vec<String>, AgentSpawnPreflightError> {
    let Some(value) = object.get("capabilities") else {
        return default_capabilities(registry, role);
    };
    if value.is_null() {
        return default_capabilities(registry, role);
    }
    let Some(values) = value.as_array() else {
        return Err(AgentSpawnPreflightError::input(
            "invalid_arguments",
            "agent_spawn capabilities 必须是数组或 null",
            "请省略 capabilities 使用角色默认能力，或传入字符串数组。",
        ));
    };
    if values.is_empty() {
        return Err(AgentSpawnPreflightError::input(
            "invalid_capabilities",
            "agent_spawn capabilities 至少需要一项专业能力",
            "请从 tool_catalog 返回的目标角色 capability_ids 中选择至少一项。",
        ));
    }
    let mut seen = HashSet::new();
    let mut requested = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let Some(value) = value.as_str() else {
            return Err(AgentSpawnPreflightError::input(
                "invalid_capabilities",
                format!("agent_spawn capabilities[{index}] 必须是非空字符串"),
                "请只传入字符串形式的 capability id。",
            ));
        };
        let value = value.trim();
        if value.is_empty() || !seen.insert(value.to_string()) {
            return Err(AgentSpawnPreflightError::input(
                "invalid_capabilities",
                format!("agent_spawn capabilities[{index}] 不能为空且不能重复"),
                "请从目标角色能力集合中选择不重复的 capability id。",
            ));
        }
        requested.push(value.to_string());
    }
    registry
        .validate_capability_ids_for_role(role, &requested)
        .map_err(|error| {
            AgentSpawnPreflightError::input(
                "unsupported_capability",
                error,
                format!("请改用角色 {role} 拥有的 capability_ids。"),
            )
        })
}

fn default_capabilities(
    registry: &magi_agent_role::AgentRoleRegistry,
    role: &str,
) -> Result<Vec<String>, AgentSpawnPreflightError> {
    let defaults = registry.capability_ids_for_role(role);
    if defaults.is_empty() {
        return Err(AgentSpawnPreflightError::input(
            "invalid_capabilities",
            format!("角色 {role} 没有可激活的专业能力"),
            "请先为角色配置至少一项有效能力，再重新派发。",
        ));
    }
    registry
        .validate_capability_ids_for_role(role, &defaults)
        .map_err(|error| {
            AgentSpawnPreflightError::input(
                "invalid_capabilities",
                error,
                format!("请检查角色 {role} 的能力配置后重新派发。"),
            )
        })
}

fn parse_task_kind(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<TaskKind, AgentSpawnPreflightError> {
    let Some(value) = object.get("task_kind") else {
        return Ok(TaskKind::LocalAgent);
    };
    let Some(value) = value.as_str() else {
        return Err(AgentSpawnPreflightError::input(
            "invalid_arguments",
            "agent_spawn task_kind 必须是字符串",
            "请使用 action、validation、repair 或 work_package。",
        ));
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "action" | "validation" | "repair" | "work_package" | "workpackage" => {
            Ok(TaskKind::LocalAgent)
        }
        other => Err(AgentSpawnPreflightError::input(
            "invalid_arguments",
            format!("不支持的 agent_spawn task_kind: {other}"),
            "请使用 action、validation、repair 或 work_package。",
        )),
    }
}

fn parse_working_dir(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<PathBuf>, AgentSpawnPreflightError> {
    let Some(value) = object.get("working_dir") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(value) = value.as_str() else {
        return Err(AgentSpawnPreflightError::input(
            "invalid_arguments",
            "agent_spawn working_dir 必须是绝对路径字符串",
            "请省略 working_dir，或传入当前 workspace 内的绝对路径。",
        ));
    };
    let path = PathBuf::from(value.trim());
    if !path.is_absolute() {
        return Err(AgentSpawnPreflightError::input(
            "workspace_preflight_failed",
            "agent_spawn working_dir 必须是绝对路径",
            "请使用当前 workspace 内的绝对路径。",
        ));
    }
    Ok(Some(path))
}

fn validate_execution_chain(
    parent_task: &Task,
    task_store: &TaskStore,
    session_store: &SessionStore,
    session_id: &magi_core::SessionId,
    workspace_id: &Option<WorkspaceId>,
    agent_role_registry: &magi_agent_role::AgentRoleRegistry,
) -> Result<(), AgentSpawnPreflightError> {
    if parent_task.parent_task_id.is_some()
        || parent_task.root_task_id != parent_task.task_id
        || task_role_is_not_root_coordinator(parent_task, agent_role_registry)
    {
        return Err(AgentSpawnPreflightError::runtime(
            "parent_not_root_coordinator",
            "execution_context",
            "只有当前任务树根节点的 coordinator 可以创建子代理",
            "请回到 root coordinator 主线派发代理，不要在 Worker 中继续创建子代理。",
        ));
    }
    let Some(chain) = session_store.active_execution_chain(session_id) else {
        return Err(AgentSpawnPreflightError::runtime(
            "active_execution_chain_missing",
            "execution_context",
            "agent_spawn 需要当前会话存在活跃执行链",
            "请在当前主线 Turn 中重新派发代理。",
        ));
    };
    let parent_in_store = task_store.get_task(&parent_task.task_id);
    if parent_in_store.as_ref().is_none_or(|task| {
        task.mission_id != parent_task.mission_id || task.root_task_id != parent_task.root_task_id
    }) {
        return Err(AgentSpawnPreflightError::runtime(
            "active_execution_chain_missing",
            "execution_context",
            "agent_spawn 父任务已不属于当前执行链",
            "请使用当前主线任务重新派发，不要复用旧任务上下文。",
        ));
    }
    if chain.session_id != *session_id
        || chain.mission_id != parent_task.mission_id
        || chain.root_task_id != parent_task.root_task_id
        || !chain
            .branches
            .iter()
            .any(|branch| branch.task_id == parent_task.task_id)
    {
        return Err(AgentSpawnPreflightError::runtime(
            "active_execution_chain_missing",
            "execution_context",
            "agent_spawn 父任务不在当前活跃执行链中",
            "请在当前主线 Turn 中重新派发代理。",
        ));
    }
    if chain.workspace_id != *workspace_id {
        return Err(AgentSpawnPreflightError::runtime(
            "workspace_preflight_failed",
            "workspace",
            "agent_spawn 的 workspace scope 与当前执行链不一致",
            "请回到当前 workspace 后重新派发代理。",
        ));
    }
    if parent_in_store.is_some_and(|task| {
        matches!(
            task.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
        )
    }) {
        return Err(AgentSpawnPreflightError::runtime(
            "active_execution_chain_missing",
            "execution_context",
            "agent_spawn 父任务已经进入终态",
            "请在新的主线任务中重新派发代理。",
        ));
    }
    Ok(())
}

fn task_role_is_not_root_coordinator(
    task: &Task,
    agent_role_registry: &magi_agent_role::AgentRoleRegistry,
) -> bool {
    !task_is_coordinator(Some(task), Some(agent_role_registry))
        || task.executor_binding_target_role() != Some("coordinator")
}

fn preflight_model(
    execution_registry: &TaskExecutionRegistry,
    parent_task: &Task,
    runtime_settings: &Option<Arc<SettingsStore>>,
    default_model_client: Option<Arc<dyn ModelBridgeClient>>,
    role: &str,
    session_id: &magi_core::SessionId,
) -> Result<Option<String>, AgentSpawnPreflightError> {
    let parent_settings = execution_registry
        .get(&parent_task.task_id)
        .and_then(|plan| plan.execution_settings_snapshot());
    let settings = parent_settings.as_ref().or(runtime_settings.as_ref());
    if settings.is_none() && default_model_client.is_none() {
        // 轻量单测和仅验证任务拓扑的运行时没有模型配置注入，保留其原有能力；daemon
        // 生产路径会注入 settings 或默认模型客户端，从而启用完整模型预检。
        return Ok(None);
    }
    let role_bound = settings
        .map(|store| configured_role_engine_model_config(store, role))
        .transpose()
        .map_err(|error| {
            tracing::warn!(role, %error, "agent_spawn role model preflight failed");
            AgentSpawnPreflightError::runtime(
                "model_preflight_failed",
                "model",
                "角色模型配置不可用",
                "请检查角色模型引擎绑定和编排模型配置后重试。",
            )
        })?
        .flatten();
    if role_bound.is_some() {
        let resolved = resolve_target_for_role(
            settings,
            default_model_client,
            RoleTarget::Agent { role_id: role },
            Some(session_id),
        )
        .map_err(|error| {
            tracing::warn!(role, %error, "agent_spawn bound role model preflight failed");
            AgentSpawnPreflightError::runtime(
                "model_preflight_failed",
                "model",
                "角色模型配置不可用",
                "请检查角色模型引擎绑定后重试。",
            )
        })?;
        if resolved.is_none() {
            return Err(AgentSpawnPreflightError::runtime(
                "model_preflight_failed",
                "model",
                "角色模型配置不可用",
                "请检查角色模型引擎绑定后重试。",
            ));
        }
        return Ok(Some("role_engine".to_string()));
    }
    let resolved = resolve_target_for_role(
        settings,
        default_model_client,
        RoleTarget::Orchestrator,
        Some(session_id),
    )
    .map_err(|error| {
        tracing::warn!(role, %error, "agent_spawn inherited model preflight failed");
        AgentSpawnPreflightError::runtime(
            "model_preflight_failed",
            "model",
            "编排模型配置不可用",
            "请检查主对话编排模型配置后重试。",
        )
    })?;
    if resolved.is_none() {
        return Err(AgentSpawnPreflightError::runtime(
            "model_preflight_failed",
            "model",
            "编排模型配置不可用",
            "请检查主对话编排模型配置后重试。",
        ));
    }
    Ok(Some("inherited_orchestrator".to_string()))
}

fn preflight_workspace_and_git(
    execution_registry: &TaskExecutionRegistry,
    parent_task: &Task,
    session_id: &magi_core::SessionId,
    workspace_id: &Option<WorkspaceId>,
    working_dir: Option<&PathBuf>,
    runtime: &crate::task_execution_registry::AgentSpawnPreflightRuntime,
) -> Result<(), AgentSpawnPreflightError> {
    let parent_root = execution_registry
        .get(&parent_task.task_id)
        .and_then(|plan| match plan {
            TaskExecutionPlan::Dispatch { execution_root, .. } => execution_root,
        });
    let context = runtime
        .session_code_contexts
        .as_ref()
        .and_then(|registry| registry.get(session_id.as_str()));
    let workspace_root = context
        .as_ref()
        .map(|context| context.execution_root.clone())
        .or_else(|| {
            workspace_id.as_ref().and_then(|workspace_id| {
                runtime.workspace_registry.as_ref().and_then(|registry| {
                    registry
                        .workspaces()
                        .into_iter()
                        .find(|workspace| workspace.workspace_id == *workspace_id)
                        .map(|workspace| workspace.native_root_path())
                })
            })
        })
        .or(parent_root);

    let runtime_has_workspace_facts = runtime.session_code_contexts.is_some()
        || runtime.workspace_registry.is_some()
        || runtime.git_service_configured;
    if runtime_has_workspace_facts {
        if let Some(root) = workspace_root.as_ref() {
            if !root.is_dir() {
                return Err(AgentSpawnPreflightError::runtime(
                    "workspace_preflight_failed",
                    "workspace",
                    "当前 workspace 执行目录不可用",
                    "请检查 workspace 路径后重试。",
                ));
            }
        } else if workspace_id.is_some() {
            return Err(AgentSpawnPreflightError::runtime(
                "workspace_preflight_failed",
                "workspace",
                "当前 workspace 缺少可用执行目录",
                "请重新打开 workspace 后重试。",
            ));
        }
    }
    if let Some(working_dir) = working_dir {
        if !working_dir.is_dir() {
            return Err(AgentSpawnPreflightError::runtime(
                "workspace_preflight_failed",
                "workspace",
                "agent_spawn working_dir 不存在或不是目录",
                "请使用当前 workspace 内已经存在的目录。",
            ));
        }
        if let Some(root) = workspace_root.as_ref() {
            if !working_dir.starts_with(root) {
                return Err(AgentSpawnPreflightError::runtime(
                    "workspace_preflight_failed",
                    "workspace",
                    "agent_spawn working_dir 超出当前 workspace 边界",
                    "请使用当前 workspace 内的目录。",
                ));
            }
        }
    }
    if let Some(context) = context {
        if workspace_id
            .as_ref()
            .is_some_and(|workspace_id| context.workspace_id != workspace_id.as_str())
        {
            return Err(AgentSpawnPreflightError::runtime(
                "git_preflight_failed",
                "git",
                "session Git context 与当前 workspace 不一致",
                "请刷新当前 workspace 的 Git 状态后重试。",
            ));
        }
        if context.has_external_drift() {
            return Err(AgentSpawnPreflightError::runtime(
                "git_preflight_failed",
                "git",
                "session Git context 已发生外部变化",
                "请刷新或恢复当前 workspace 的 Git 状态后重试。",
            ));
        }
        if context.git.base_head.is_none() || !context.git.worktree_path.is_dir() {
            return Err(AgentSpawnPreflightError::runtime(
                "git_preflight_failed",
                "git",
                "当前 Git session 缺少可用 base HEAD 或 worktree",
                "请重新建立 Git session 后重试。",
            ));
        }
        if !runtime.git_service_configured {
            return Err(AgentSpawnPreflightError::runtime(
                "git_preflight_failed",
                "git",
                "Git service 尚未就绪，无法隔离子代理 workspace",
                "请稍后重试，或检查 daemon Git 运行时。",
            ));
        }
    }
    Ok(())
}

pub(crate) fn valid_agent_task_name(task_name: &str) -> bool {
    (1..=48).contains(&task_name.chars().count())
        && task_name.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

pub(crate) fn child_canonical_task_name(parent_name: &str, task_name: &str) -> String {
    let parent_name = parent_name.trim().trim_end_matches('/');
    let parent_name = if parent_name.is_empty() {
        "/root"
    } else {
        parent_name
    };
    format!("{parent_name}/{task_name}")
}

pub(crate) fn agent_spawn_child_policy_snapshot(parent_policy: Option<&TaskPolicy>) -> TaskPolicy {
    parent_policy
        .cloned()
        .unwrap_or_else(default_agent_spawn_policy)
}

pub(crate) fn agent_spawn_child_dependency_ids(parent: &Task) -> Vec<TaskId> {
    parent.dependency_ids.clone()
}

fn default_agent_spawn_policy() -> TaskPolicy {
    TaskPolicy {
        autonomy_level: "Autonomous".to_string(),
        access_profile: AccessProfile::Restricted,
        collaboration_mode: Default::default(),
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
        allowed_paths: Vec::new(),
        denied_paths: Vec::new(),
        read_only_paths: Vec::new(),
        network_mode: "full".to_string(),
        command_mode: "full".to_string(),
        retry_limit: 1,
        validation_profile: None,
        checkpoint_mode: "turn".to_string(),
        task_tier: TaskTier::ExecutionChain,
        background_allowed: true,
        escalation_conditions: Vec::new(),
    }
}

pub(crate) fn parse_agent_context_package(
    parsed: &serde_json::Value,
    parent_task_id: &TaskId,
    now: UtcMillis,
    sequence: u64,
) -> Result<AgentContextPackage, String> {
    let context_package = match parsed.get("context_package") {
        Some(serde_json::Value::Object(_)) => parsed
            .get("context_package")
            .cloned()
            .expect("context_package object should remain present"),
        Some(serde_json::Value::String(encoded)) => {
            serde_json::from_str(encoded).map_err(|_| {
                "agent_spawn 的 context_package 必须是对象或可解析为对象的 JSON 字符串".to_string()
            })?
        }
        Some(_) => return Err("agent_spawn 的 context_package 必须是结构化对象".to_string()),
        None => return Err("agent_spawn 缺少结构化 context_package".to_string()),
    };
    let value = context_package
        .as_object()
        .ok_or_else(|| "agent_spawn 的 context_package 必须是结构化对象".to_string())?;
    for key in value.keys() {
        if !matches!(
            key.as_str(),
            "summary" | "constraints" | "expected_output" | "references"
        ) {
            return Err(format!("context_package 不支持字段 {key}"));
        }
    }
    let summary = required_bounded_context_text(
        value.get("summary"),
        "context_package.summary",
        AGENT_CONTEXT_SUMMARY_MAX_CHARS,
    )?;
    let expected_output = required_bounded_context_text(
        value.get("expected_output"),
        "context_package.expected_output",
        AGENT_CONTEXT_EXPECTED_OUTPUT_MAX_CHARS,
    )?;
    let constraints = value
        .get("constraints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "context_package.constraints 必须是数组".to_string())?
        .iter()
        .enumerate()
        .map(|(index, item)| {
            required_bounded_context_text(
                Some(item),
                &format!("context_package.constraints[{index}]"),
                AGENT_CONTEXT_CONSTRAINT_MAX_CHARS,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let references =
        parse_agent_context_references(value.get("references"), now, sequence, "spawn")?;
    Ok(AgentContextPackage {
        package_id: format!(
            "agent-context-{}-{}-{sequence}",
            parent_task_id.as_str(),
            now.0
        ),
        revision: 1,
        parent_task_id: parent_task_id.clone(),
        summary,
        constraints,
        expected_output,
        references,
        supplements: Vec::new(),
        created_at: now,
        updated_at: now,
    })
}

pub(crate) fn parse_agent_context_references(
    value: Option<&serde_json::Value>,
    now: UtcMillis,
    sequence: u64,
    scope: &str,
) -> Result<Vec<AgentContextReference>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("{scope}.references 必须是数组"))?;
    if values.len() > AGENT_CONTEXT_REFERENCE_LIMIT {
        return Err(format!(
            "{scope}.references 最多允许 {AGENT_CONTEXT_REFERENCE_LIMIT} 条"
        ));
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let object = value
                .as_object()
                .ok_or_else(|| format!("{scope}.references[{index}] 必须是对象"))?;
            for key in object.keys() {
                if !matches!(key.as_str(), "kind" | "title" | "source_ref" | "preview") {
                    return Err(format!("{scope}.references[{index}] 不支持字段 {key}"));
                }
            }
            let kind = parse_agent_context_reference_kind(
                object
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            )?;
            let source_ref = required_bounded_context_text(
                object.get("source_ref"),
                &format!("{scope}.references[{index}].source_ref"),
                1_000,
            )?;
            let title = optional_bounded_context_text(
                object.get("title"),
                &format!("{scope}.references[{index}].title"),
                200,
            )?
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| default_agent_context_reference_title(&source_ref));
            let preview = optional_bounded_context_text(
                object.get("preview"),
                &format!("{scope}.references[{index}].preview"),
                AGENT_CONTEXT_PREVIEW_MAX_CHARS,
            )?
            .unwrap_or_default();
            Ok(AgentContextReference {
                reference_id: format!("ctxref-{scope}-{}-{sequence}-{index}", now.0),
                kind,
                title,
                source_ref,
                estimated_tokens: estimate_text_tokens(&preview),
                preview,
            })
        })
        .collect()
}

fn parse_agent_context_reference_kind(value: &str) -> Result<AgentContextReferenceKind, String> {
    match value.trim() {
        "conversation_turn" => Ok(AgentContextReferenceKind::ConversationTurn),
        "task_output" => Ok(AgentContextReferenceKind::TaskOutput),
        "task_evidence" => Ok(AgentContextReferenceKind::TaskEvidence),
        "file" => Ok(AgentContextReferenceKind::File),
        "knowledge" => Ok(AgentContextReferenceKind::Knowledge),
        "other" => Ok(AgentContextReferenceKind::Other),
        _ => Err(format!("未知上下文引用类型: {value}")),
    }
}

pub(crate) fn required_bounded_context_text(
    value: Option<&serde_json::Value>,
    field: &str,
    max_chars: usize,
) -> Result<String, String> {
    let text = bounded_context_text(value, field, max_chars)?;
    if text.is_empty() {
        Err(format!("{field} 不能为空"))
    } else {
        Ok(text)
    }
}

pub(crate) fn optional_bounded_context_text(
    value: Option<&serde_json::Value>,
    field: &str,
    max_chars: usize,
) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    bounded_context_text(Some(value), field, max_chars).map(Some)
}

fn default_agent_context_reference_title(source_ref: &str) -> String {
    let mut title = source_ref.trim().chars().take(200).collect::<String>();
    if source_ref.trim().chars().count() > 200 {
        title.push('…');
    }
    title
}

fn bounded_context_text(
    value: Option<&serde_json::Value>,
    field: &str,
    max_chars: usize,
) -> Result<String, String> {
    let text = value
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{field} 必须是字符串"))?
        .trim()
        .to_string();
    if text.chars().count() > max_chars {
        return Err(format!("{field} 最多允许 {max_chars} 个字符"));
    }
    Ok(text)
}
