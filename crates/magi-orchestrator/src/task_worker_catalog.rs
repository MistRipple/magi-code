use magi_agent_role::AgentRoleRegistry;
use magi_core::{Task, TaskKind, WorkerId};

/// Describes a worker's capabilities for task matching.
#[derive(Clone, Debug)]
pub struct WorkerInfo {
    pub worker_id: WorkerId,
    /// The role this worker can fulfil (e.g. "executor", "reviewer", "explorer").
    pub role: String,
    /// Task kinds this worker is capable of handling.
    pub supported_kinds: Vec<TaskKind>,
    /// Maximum number of concurrent tasks this worker can handle (design 5.4).
    /// None means unlimited.
    pub parallelism_limit: Option<u32>,
    /// Role-specific system prompt template injected at LLM invocation (design 8.1).
    pub system_prompt_template: Option<String>,
}

/// 任务系统：role / prompt 经 `AgentRoleRegistry` 解析；本模块只保留目录构造与角色解析。
pub fn default_task_role_for_kind(kind: TaskKind) -> Option<&'static str> {
    match kind {
        TaskKind::LocalAgent => Some("executor"),
        TaskKind::LocalWorkflow => Some("executor"),
        TaskKind::RemoteAgent => Some("executor"),
        TaskKind::MonitorMcp => Some("executor"),
        TaskKind::InProcessTeammate => Some("executor"),
        TaskKind::Dream => Some("architect"),
    }
}

pub fn resolve_task_role<'a>(task: &'a Task, registry: &AgentRoleRegistry) -> Option<&'a str> {
    if let Some(role) = task.executor_binding_target_role() {
        // 任务已经显式绑定角色时，非法或不支持当前 TaskKind 的角色必须让
        // 调度层明确返回“无匹配”，不能静默改派到 executor 掩盖配置错误。
        return registry
            .role_supports_task_kind(role, task.kind)
            .then_some(role);
    }
    default_task_role_for_kind(task.kind)
}

pub fn build_worker_info_for_role(
    registry: &AgentRoleRegistry,
    role_id: &str,
) -> Option<WorkerInfo> {
    let role = registry.get(role_id)?;
    let supported_kinds = role.supported_task_kinds();
    if supported_kinds.is_empty() {
        return None;
    }
    Some(WorkerInfo {
        worker_id: WorkerId::new(format!("task-worker-{role_id}")),
        role: role_id.to_string(),
        supported_kinds,
        parallelism_limit: role.parallelism_limit,
        system_prompt_template: Some(role.system_prompt.clone()),
    })
}

pub fn build_worker_catalog_for_roles<I, S>(
    registry: &AgentRoleRegistry,
    roles: I,
) -> Vec<WorkerInfo>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut workers = Vec::new();
    for role in roles {
        let role = role.as_ref();
        if workers
            .iter()
            .any(|worker: &WorkerInfo| worker.role == role)
        {
            continue;
        }
        if let Some(worker) = build_worker_info_for_role(registry, role) {
            workers.push(worker);
        }
    }
    workers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> AgentRoleRegistry {
        AgentRoleRegistry::load_default()
    }

    #[test]
    fn resolve_task_role_uses_bound_role_when_it_supports_kind() {
        let reg = registry();
        let task = Task {
            task_id: magi_core::TaskId::new("t-1"),
            mission_id: magi_core::MissionId::new("m-1"),
            root_task_id: magi_core::TaskId::new("t-1"),
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "check TEST workspace".to_string(),
            goal: "inspect /Users/xie/code/TEST".to_string(),
            status: magi_core::TaskStatus::Pending,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: Some(magi_core::TaskExecutorBinding::for_role("tester")),
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: magi_core::UtcMillis::now(),
            updated_at: magi_core::UtcMillis::now(),
        };

        assert_eq!(resolve_task_role(&task, &reg), Some("tester"));
    }

    #[test]
    fn coordinator_task_resolves_to_coordinator_worker() {
        let reg = registry();
        let mut task = Task {
            task_id: magi_core::TaskId::new("t-coordinator"),
            mission_id: magi_core::MissionId::new("m-coordinator"),
            root_task_id: magi_core::TaskId::new("t-coordinator"),
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "coordinate agents".to_string(),
            goal: "spawn multiple agents".to_string(),
            status: magi_core::TaskStatus::Pending,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: magi_core::UtcMillis::now(),
            updated_at: magi_core::UtcMillis::now(),
        };
        task.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));

        let role = resolve_task_role(&task, &reg).expect("coordinator role should resolve");
        let candidates = build_worker_catalog_for_roles(&reg, [role]);

        assert_eq!(role, "coordinator");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].role, "coordinator");
    }
}
