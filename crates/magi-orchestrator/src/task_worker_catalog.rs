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
    if let Some(binding) = task.executor_binding.as_ref() {
        let role = binding.target_role.trim();
        if !role.is_empty() && role_supports_task_kind(role, task.kind) {
            return Some(role);
        }
    }
    default_task_role_for_kind(task.kind)
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

pub fn role_supports_task_kind(role: &str, kind: TaskKind) -> bool {
    supported_kinds_for_role(role).contains(&kind)
}

pub fn compatible_task_role_for_kind(kind: TaskKind, candidate: Option<&str>) -> Option<String> {
    if let Some(role) = candidate.map(str::trim).filter(|role| !role.is_empty()) {
        if role_supports_task_kind(role, kind) {
            return Some(role.to_string());
        }
    }
    default_task_role_for_kind(kind).map(ToOwned::to_owned)
}

pub fn build_worker_info_for_role(role: &str) -> Option<WorkerInfo> {
    let supported_kinds = supported_kinds_for_role(role);
    if supported_kinds.is_empty() {
        return None;
    }
    let system_prompt_template = match role {
        "architect" => Some(
            "你是系统架构师。你的职责是理解高层目标，将其分解为可执行的 Phase 和 WorkPackage。\
重点关注模块边界、接口契约和依赖关系。输出必须包含清晰的任务分解和验收标准。\
你同时是面向用户的唯一代言人：如果执行过程中的 worker 遇到阻碍性问题需要用户决策，\
你必须先收集其上下文并改写为面向用户的明确询问，再在主线向用户请求决策，不得让 worker \
绕过你直接与用户互动。".to_string(),
        ),
        "integration-dev" => Some(
            "你是全栈集成开发工程师。你的职责是执行具体的 WorkPackage 和 Action，编写代码、修复缺陷、运行测试。\
遵循项目编码规范，确保代码可编译、测试通过。输出必须包含修改的文件列表和关键代码片段。".to_string(),
        ),
        "reviewer" => Some(
            "你是代码审查与验证工程师。你的职责是验证任务输出是否符合验收标准，检查代码质量、安全性和可维护性。\
对发现的问题给出明确的通过/不通过结论及修复建议。".to_string(),
        ),
        "debugger" => Some(
            "你是调试与修复工程师。你的职责是分析失败原因，定位根因，实施修复并验证。\
优先最小化改动范围，避免引入回归。输出必须包含根因分析和修复方案。".to_string(),
        ),
        "frontend-dev" => Some(
            "你是前端开发工程师。你的职责是实现用户界面、交互逻辑和前端状态管理。\
确保响应式设计、可访问性和性能。输出必须包含组件结构和关键样式/逻辑代码。".to_string(),
        ),
        "backend-dev" => Some(
            "你是后端开发工程师。你的职责是实现 API、业务逻辑、数据访问层和基础设施代码。\
确保接口稳定、数据一致性和安全性。输出必须包含接口定义和关键业务逻辑代码。".to_string(),
        ),
        _ => None,
    };
    Some(WorkerInfo {
        worker_id: WorkerId::new(format!("task-worker-{role}")),
        role: role.to_string(),
        supported_kinds,
        parallelism_limit: None,
        system_prompt_template,
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
    fn resolve_task_role_falls_back_when_bound_role_cannot_execute_kind() {
        let task = Task {
            task_id: magi_core::TaskId::new("t-1"),
            mission_id: magi_core::MissionId::new("m-1"),
            root_task_id: magi_core::TaskId::new("t-1"),
            parent_task_id: None,
            kind: TaskKind::Action,
            title: "check TEST workspace".to_string(),
            goal: "inspect /Users/xie/code/TEST".to_string(),
            status: magi_core::TaskStatus::Ready,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: Some(magi_core::ExecutorBinding {
                target_role: "test-engineer".to_string(),
                capability_requirements: Vec::new(),
                parallelism_group: None,
                exclusive_scope: None,
                worker_selector: None,
            }),
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

        assert_eq!(resolve_task_role(&task), Some("integration-dev"));
    }

    #[test]
    fn register_custom_worker() {
        let catalog = DynamicWorkerCatalog::new();
        catalog.register_custom(
            WorkerId::new("gpu-worker-1"),
            "ml-engineer".to_string(),
            vec![TaskKind::Action, TaskKind::Validation],
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
