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
use std::collections::HashMap;
use std::sync::RwLock;

/// 任务系统：role / prompt 经 `AgentRoleRegistry` 解析；本模块只保留动态目录查询。
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
        if registry.role_supports_task_kind(role, task.kind) {
            return Some(role);
        }
    }
    default_task_role_for_kind(task.kind)
}

pub fn supported_kinds_for_role(registry: &AgentRoleRegistry, role: &str) -> Vec<TaskKind> {
    registry.supported_task_kinds(role)
}

pub fn role_supports_task_kind(registry: &AgentRoleRegistry, role: &str, kind: TaskKind) -> bool {
    registry.role_supports_task_kind(role, kind)
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
// --- 动态 Worker 目录：支持运行时注册/注销/查询

pub struct DynamicWorkerCatalog {
    workers: RwLock<HashMap<String, WorkerInfo>>,
}

impl DynamicWorkerCatalog {
    pub fn new() -> Self {
        Self {
            workers: RwLock::new(HashMap::new()),
        }
    }

    /// 由注册表中所有 role 一次性填充。启动路径调用 `AgentRoleRegistry::load_default()`
    /// 拿到注册表后再传入这里，DynamicWorkerCatalog 自此不再持有硬编码 role 列表。
    pub fn with_registry(registry: &AgentRoleRegistry) -> Self {
        let catalog = Self::new();
        for role in registry.all() {
            if let Some(worker) = build_worker_info_for_role(registry, &role.id) {
                catalog.register(worker);
            }
        }
        catalog
    }

    pub fn register(&self, worker: WorkerInfo) {
        let mut workers = self.workers.write().expect("catalog write lock poisoned");
        workers.insert(worker.worker_id.to_string(), worker);
    }

    pub fn register_custom(
        &self,
        worker_id: WorkerId,
        role: String,
        supported_kinds: Vec<TaskKind>,
        parallelism_limit: Option<u32>,
        system_prompt_template: Option<String>,
    ) {
        self.register(WorkerInfo {
            worker_id,
            role,
            supported_kinds,
            parallelism_limit,
            system_prompt_template,
        });
    }

    pub fn deregister(&self, worker_id: &WorkerId) -> Option<WorkerInfo> {
        let mut workers = self.workers.write().expect("catalog write lock poisoned");
        workers.remove(worker_id.as_str())
    }

    pub fn get(&self, worker_id: &WorkerId) -> Option<WorkerInfo> {
        let workers = self.workers.read().expect("catalog read lock poisoned");
        workers.get(worker_id.as_str()).cloned()
    }

    pub fn find_by_role(&self, role: &str) -> Vec<WorkerInfo> {
        let workers = self.workers.read().expect("catalog read lock poisoned");
        workers
            .values()
            .filter(|w| w.role == role)
            .cloned()
            .collect()
    }

    pub fn find_for_task(&self, task: &Task, registry: &AgentRoleRegistry) -> Vec<WorkerInfo> {
        let required_role = resolve_task_role(task, registry);
        let workers = self.workers.read().expect("catalog read lock poisoned");
        workers
            .values()
            .filter(|w| {
                let role_match = match required_role {
                    Some(role) => w.role == role,
                    None => false,
                };
                let kind_match = w.supported_kinds.contains(&task.kind);
                role_match || kind_match
            })
            .cloned()
            .collect()
    }

    pub fn all_workers(&self) -> Vec<WorkerInfo> {
        let workers = self.workers.read().expect("catalog read lock poisoned");
        workers.values().cloned().collect()
    }

    pub fn worker_count(&self) -> usize {
        let workers = self.workers.read().expect("catalog read lock poisoned");
        workers.len()
    }
}

impl Default for DynamicWorkerCatalog {
    fn default() -> Self {
        Self::with_registry(&AgentRoleRegistry::load_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> AgentRoleRegistry {
        AgentRoleRegistry::load_default()
    }

    #[test]
    fn default_catalog_has_workers() {
        let catalog = DynamicWorkerCatalog::default();
        assert!(catalog.worker_count() >= 4);
    }

    #[test]
    fn register_and_deregister() {
        let catalog = DynamicWorkerCatalog::new();
        assert_eq!(catalog.worker_count(), 0);

        let worker = WorkerInfo {
            worker_id: WorkerId::new("custom-1"),
            role: "ml-engineer".to_string(),
            supported_kinds: vec![TaskKind::LocalAgent],
            parallelism_limit: Some(2),
            system_prompt_template: None,
        };
        catalog.register(worker);
        assert_eq!(catalog.worker_count(), 1);
        assert!(catalog.get(&WorkerId::new("custom-1")).is_some());

        let removed = catalog.deregister(&WorkerId::new("custom-1"));
        assert!(removed.is_some());
        assert_eq!(catalog.worker_count(), 0);
    }

    #[test]
    fn find_by_role() {
        let catalog = DynamicWorkerCatalog::with_registry(&registry());
        let architects = catalog.find_by_role("architect");
        assert!(!architects.is_empty());
        assert!(architects.iter().all(|w| w.role == "architect"));
    }

    #[test]
    fn find_for_task() {
        let reg = registry();
        let catalog = DynamicWorkerCatalog::with_registry(&reg);
        let task = Task {
            task_id: magi_core::TaskId::new("t-1"),
            mission_id: magi_core::MissionId::new("m-1"),
            root_task_id: magi_core::TaskId::new("t-1"),
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "test".to_string(),
            goal: "test".to_string(),
            status: magi_core::TaskStatus::Pending,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
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
        let candidates = catalog.find_for_task(&task, &reg);
        assert!(!candidates.is_empty());
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
    fn register_custom_worker() {
        let catalog = DynamicWorkerCatalog::new();
        catalog.register_custom(
            WorkerId::new("gpu-worker-1"),
            "ml-engineer".to_string(),
            vec![TaskKind::LocalAgent, TaskKind::LocalAgent],
            Some(4),
            Some("GPU 加速机器学习工程师提示词".to_string()),
        );
        let w = catalog.get(&WorkerId::new("gpu-worker-1")).unwrap();
        assert_eq!(w.role, "ml-engineer");
        assert_eq!(w.parallelism_limit, Some(4));
        assert_eq!(w.supported_kinds.len(), 2);
        assert_eq!(
            w.system_prompt_template,
            Some("GPU 加速机器学习工程师提示词".to_string())
        );
    }
}
