use crate::task_runner::WorkerInfo;
use magi_core::{Task, TaskKind, WorkerId};

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
        if workers.iter().any(|worker: &WorkerInfo| worker.role == role) {
            continue;
        }
        if let Some(worker) = build_worker_info_for_role(role) {
            workers.push(worker);
        }
    }
    workers
}
