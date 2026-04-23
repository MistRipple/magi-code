use crate::task_runner::WorkerInfo;
use magi_core::{Task, TaskKind, WorkerId};
use std::collections::HashMap;
use std::sync::RwLock;

pub fn default_task_role_for_kind(kind: TaskKind) -> Option<&'static str> {
    match kind {
        TaskKind::Objective | TaskKind::Phase => Some("architect"),
        TaskKind::WorkPackage | TaskKind::Action => Some("integration-dev"),
        TaskKind::Validation => Some("reviewer"),
        TaskKind::Repair => Some("debugger"),
        TaskKind::Decision => None,
    }
}

pub fn resolve_task_role(task: &Task) -> Option<&str> {
    task.executor_binding
        .as_ref()
        .map(|binding| binding.target_role.as_str())
        .or_else(|| default_task_role_for_kind(task.kind))
}

pub fn supported_kinds_for_role(role: &str) -> Vec<TaskKind> {
    match role {
        "architect" => vec![TaskKind::Objective, TaskKind::Phase, TaskKind::WorkPackage],
        "frontend-dev" | "backend-dev" | "integration-dev" | "data-engineer"
        | "devops-engineer" | "security-analyst" | "doc-writer" => {
            vec![TaskKind::WorkPackage, TaskKind::Action]
        }
        "reviewer" | "test-engineer" => vec![TaskKind::Validation],
        "debugger" => vec![TaskKind::Repair, TaskKind::Action],
        _ => Vec::new(),
    }
}

pub fn build_worker_info_for_role(role: &str) -> Option<WorkerInfo> {
    let supported_kinds = supported_kinds_for_role(role);
    if supported_kinds.is_empty() {
        return None;
    }
    Some(WorkerInfo {
        worker_id: WorkerId::new(format!("task-worker-{role}")),
        role: role.to_string(),
        supported_kinds,
        parallelism_limit: None,
    })
}

pub fn build_worker_catalog_for_roles<I, S>(roles: I) -> Vec<WorkerInfo>
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
        if let Some(worker) = build_worker_info_for_role(role) {
            workers.push(worker);
        }
    }
    workers
}

// ---------------------------------------------------------------------------
// 动态 Worker 目录：支持运行时注册/注销/查询
// ---------------------------------------------------------------------------

pub struct DynamicWorkerCatalog {
    workers: RwLock<HashMap<String, WorkerInfo>>,
}

impl DynamicWorkerCatalog {
    pub fn new() -> Self {
        Self {
            workers: RwLock::new(HashMap::new()),
        }
    }

    pub fn with_default_roles() -> Self {
        let catalog = Self::new();
        let default_roles = [
            "architect",
            "integration-dev",
            "frontend-dev",
            "backend-dev",
            "reviewer",
            "debugger",
        ];
        for role in default_roles {
            if let Some(worker) = build_worker_info_for_role(role) {
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
    ) {
        self.register(WorkerInfo {
            worker_id,
            role,
            supported_kinds,
            parallelism_limit,
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

    pub fn find_for_task(&self, task: &Task) -> Vec<WorkerInfo> {
        let required_role = resolve_task_role(task);
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
        Self::with_default_roles()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            supported_kinds: vec![TaskKind::Action],
            parallelism_limit: Some(2),
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
        let catalog = DynamicWorkerCatalog::with_default_roles();
        let architects = catalog.find_by_role("architect");
        assert!(!architects.is_empty());
        assert!(architects.iter().all(|w| w.role == "architect"));
    }

    #[test]
    fn find_for_task() {
        let catalog = DynamicWorkerCatalog::with_default_roles();
        let task = Task {
            task_id: magi_core::TaskId::new("t-1"),
            mission_id: magi_core::MissionId::new("m-1"),
            root_task_id: magi_core::TaskId::new("t-1"),
            parent_task_id: None,
            kind: TaskKind::Action,
            title: "test".to_string(),
            goal: "test".to_string(),
            status: magi_core::TaskStatus::Ready,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            created_at: magi_core::UtcMillis::now(),
            updated_at: magi_core::UtcMillis::now(),
        };
        let candidates = catalog.find_for_task(&task);
        assert!(!candidates.is_empty());
    }

    #[test]
    fn register_custom_worker() {
        let catalog = DynamicWorkerCatalog::new();
        catalog.register_custom(
            WorkerId::new("gpu-worker-1"),
            "ml-engineer".to_string(),
            vec![TaskKind::Action, TaskKind::Validation],
            Some(4),
        );
        let w = catalog.get(&WorkerId::new("gpu-worker-1")).unwrap();
        assert_eq!(w.role, "ml-engineer");
        assert_eq!(w.parallelism_limit, Some(4));
        assert_eq!(w.supported_kinds.len(), 2);
    }
}
