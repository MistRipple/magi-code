//! 任务系统 — 任务派发计划与注册中心。
//!
//! - [`TaskExecutionPlan`]：dispatch_submission 接受后挂在 task_execution_registry
//!   上的派发载体；当前派发链路只保留 Dispatch 一支。
//! - [`TaskExecutionRegistry`]：线程安全的 `TaskId → TaskExecutionPlan` 索引，
//!   `LlmTaskDispatcher` 与 `Runner` 通过它取出已接受派发计划。
//!
//! magi-api 不再实现这两个类型，改为 `pub use` 重导出；本模块是任务派发链路的
//! 唯一所有者。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use magi_core::{
    ExecutionOwnership, SessionId, Task, TaskExecutionTarget, TaskId, ThreadId, UtcMillis,
    WorkerId, WorkspaceId,
};
use magi_orchestrator::{ExecutionWritebackPlans, task_store::TaskStore};
use magi_session_store::{ActiveExecutionBranch, SessionStore};
use magi_settings_store::SettingsStore;
use magi_spawn_graph::SpawnGraph;

use crate::{session_images::SessionTurnImage, session_thread};

#[derive(Clone, Debug)]
pub enum TaskExecutionPlan {
    Dispatch {
        target: TaskExecutionTarget,
        worker_id: WorkerId,
        /// task 绑定的 thread，由 `session_thread::ensure_thread_for_role` 为当前 task
        /// 独立创建，是 task 详情归属与当前 task 恢复记录的路由键。
        thread_id: ThreadId,
        is_primary: bool,
        session_id: SessionId,
        workspace_id: Option<WorkspaceId>,
        ownership: ExecutionOwnership,
        writebacks: ExecutionWritebackPlans,
        use_tools: bool,
        skill_name: Option<String>,
        images: Vec<SessionTurnImage>,
        execution_settings_snapshot: Option<Arc<SettingsStore>>,
    },
}

impl TaskExecutionPlan {
    pub fn execution_settings_snapshot(&self) -> Option<Arc<SettingsStore>> {
        match self {
            Self::Dispatch {
                execution_settings_snapshot,
                ..
            } => execution_settings_snapshot.clone(),
        }
    }
}

pub struct SpawnedChildExecutionRequest<'a> {
    pub task_store: &'a TaskStore,
    pub spawn_graph: &'a Mutex<SpawnGraph>,
    pub session_store: &'a SessionStore,
    pub child_task: &'a Task,
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub role: &'a str,
    pub now: UtcMillis,
}

pub struct SpawnedChildExecution {
    pub worker_id: WorkerId,
    pub thread_id: ThreadId,
    pub execution_chain_ref: String,
}

#[derive(Clone, Default)]
pub struct TaskExecutionRegistry {
    plans: Arc<RwLock<HashMap<TaskId, TaskExecutionPlan>>>,
}

impl TaskExecutionRegistry {
    pub fn insert(&self, task_id: TaskId, plan: TaskExecutionPlan) {
        self.plans
            .write()
            .expect("task execution registry write lock poisoned")
            .insert(task_id, plan);
    }

    pub fn remove(&self, task_id: &TaskId) -> Option<TaskExecutionPlan> {
        self.plans
            .write()
            .expect("task execution registry write lock poisoned")
            .remove(task_id)
    }

    pub fn get(&self, task_id: &TaskId) -> Option<TaskExecutionPlan> {
        self.plans
            .read()
            .expect("task execution registry read lock poisoned")
            .get(task_id)
            .cloned()
    }

    pub fn register_spawned_local_agent_child(
        &self,
        request: SpawnedChildExecutionRequest<'_>,
    ) -> Result<SpawnedChildExecution, String> {
        let SpawnedChildExecutionRequest {
            task_store,
            spawn_graph,
            session_store,
            child_task,
            session_id,
            workspace_id,
            role,
            now,
        } = request;
        let mut chain = session_store
            .active_execution_chain(session_id)
            .ok_or_else(|| "agent_spawn 需要当前会话存在活跃执行链".to_string())?;
        if chain.mission_id != child_task.mission_id
            || chain.root_task_id != child_task.root_task_id
        {
            return Err(format!(
                "agent_spawn 子任务不属于当前执行链: mission/root {}:{} != {}:{}",
                child_task.mission_id,
                child_task.root_task_id,
                chain.mission_id,
                chain.root_task_id
            ));
        }
        let parent_task_id = child_task.parent_task_id.clone().ok_or_else(|| {
            format!(
                "agent_spawn 子任务 {} 缺少 parent_task_id",
                child_task.task_id
            )
        })?;
        spawn_graph
            .lock()
            .map_err(|err| format!("SpawnGraph mutex poisoned: {err}"))?
            .add_edge(
                parent_task_id.clone(),
                child_task.task_id.clone(),
                child_task.kind,
                std::time::SystemTime::now(),
            )
            .map_err(|error| format!("agent_spawn 注册 SpawnGraph 边失败: {error}"))?;
        let worker_id = WorkerId::new(format!("worker-spawn-{}", child_task.task_id.as_str()));
        let thread_id = session_thread::ensure_thread_for_role(
            session_store,
            session_id,
            &chain.mission_id,
            role,
            &worker_id,
            &child_task.task_id,
            now,
        );
        let branch = ActiveExecutionBranch {
            task_id: child_task.task_id.clone(),
            worker_id: worker_id.clone(),
            stage: "execute".to_string(),
            lease_id: None,
            execution_intent_ref: None,
            binding_lifecycle: None,
            checkpoint_stage: Some("execute".to_string()),
            next_step_index: Some(0),
            checkpoint_at: Some(now),
            resume_mode: Some("stage-restart".to_string()),
            resume_token: None,
            use_tools: true,
            skill_name: None,
            is_primary: false,
            thread_id: thread_id.clone(),
        };
        chain
            .branches
            .retain(|entry| entry.task_id != child_task.task_id);
        chain.branches.push(branch);
        chain.active_branch_task_ids = chain
            .branches
            .iter()
            .map(|entry| entry.task_id.clone())
            .collect();
        chain.active_worker_bindings = chain
            .branches
            .iter()
            .map(|entry| entry.worker_id.clone())
            .collect();
        if let Some(turn) = chain.current_turn.as_mut() {
            turn.normalize();
        }
        let execution_chain_ref = chain.execution_chain_ref.clone();
        chain.normalize();
        session_store
            .upsert_active_execution_chain(session_id.clone(), chain)
            .map_err(|error| error.to_string())?;

        let execution_settings_snapshot = self
            .get(&parent_task_id)
            .and_then(|plan| plan.execution_settings_snapshot());

        self.insert(
            child_task.task_id.clone(),
            TaskExecutionPlan::Dispatch {
                target: TaskExecutionTarget {
                    mission_id: child_task.mission_id.clone(),
                    root_task_id: child_task.root_task_id.clone(),
                    task_id: child_task.task_id.clone(),
                    requested_worker_id: Some(worker_id.clone()),
                    recovery_id: None,
                    execution_chain_ref: Some(execution_chain_ref.clone()),
                },
                worker_id: worker_id.clone(),
                thread_id: thread_id.clone(),
                is_primary: false,
                session_id: session_id.clone(),
                workspace_id: workspace_id.clone(),
                ownership: ExecutionOwnership {
                    session_id: Some(session_id.clone()),
                    workspace_id: workspace_id.clone(),
                    mission_id: Some(child_task.mission_id.clone()),
                    task_id: Some(child_task.task_id.clone()),
                    worker_id: Some(worker_id.clone()),
                    execution_chain_ref: Some(execution_chain_ref.clone()),
                },
                writebacks: ExecutionWritebackPlans::default(),
                use_tools: true,
                skill_name: None,
                images: Vec::new(),
                execution_settings_snapshot,
            },
        );
        task_store.insert_task(child_task.clone());

        Ok(SpawnedChildExecution {
            worker_id,
            thread_id,
            execution_chain_ref,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{MissionId, Task, TaskKind, TaskRuntimePayload, TaskStatus, UtcMillis};
    use magi_session_store::{
        ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
        ActiveExecutionTurn, SessionStore,
    };

    fn test_task(task_id: &str, root_task_id: &str, mission_id: &MissionId) -> Task {
        let now = UtcMillis(1_000);
        Task {
            task_id: TaskId::new(task_id),
            mission_id: mission_id.clone(),
            root_task_id: TaskId::new(root_task_id),
            parent_task_id: Some(TaskId::new(root_task_id)),
            kind: TaskKind::LocalAgent,
            title: format!("task {task_id}"),
            goal: format!("run task {task_id}"),
            status: TaskStatus::Pending,
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
            runtime_payload: TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn spawned_local_agent_child_registration_is_atomic_runtime_source() {
        use magi_settings_store::SettingsStore;

        let task_store = TaskStore::new();
        let spawn_graph = Mutex::new(SpawnGraph::new());
        let session_store = SessionStore::new();
        let registry = TaskExecutionRegistry::default();
        let session_id = SessionId::new("session-atomic-spawn");
        let workspace_id = Some(WorkspaceId::new("workspace-atomic-spawn"));
        let mission_id = MissionId::new("mission-atomic-spawn");
        let root_task_id = TaskId::new("task-root");
        let parent_worker_id = WorkerId::new("worker-parent");
        let now = UtcMillis(10_000);
        let _ = session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-atomic-spawn".to_string(),
                    workspace_id: workspace_id.clone(),
                    active_branch_task_ids: vec![root_task_id.clone()],
                    active_worker_bindings: vec![parent_worker_id.clone()],
                    branches: vec![ActiveExecutionBranch {
                        task_id: root_task_id.clone(),
                        worker_id: parent_worker_id,
                        stage: "execute".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: None,
                        checkpoint_stage: Some("execute".to_string()),
                        next_step_index: Some(0),
                        checkpoint_at: Some(now),
                        resume_mode: Some("stage-restart".to_string()),
                        resume_token: None,
                        use_tools: true,
                        skill_name: None,
                        is_primary: true,
                        thread_id: ThreadId::new("thread-atomic-spawn-parent"),
                    }],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-atomic-spawn".to_string(),
                        trimmed_text: Some("spawn child".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-atomic-spawn".to_string(),
                        turn_seq: 1,
                        accepted_at: now,
                        completed_at: None,
                        status: "running".to_string(),
                        user_message: Some("spawn child".to_string()),
                        items: Vec::new(),
                    }),
                },
            )
            .expect("active chain should be accepted");

        let parent_settings_snapshot = Arc::new(SettingsStore::new().execution_snapshot());
        registry.insert(
            root_task_id.clone(),
            TaskExecutionPlan::Dispatch {
                target: magi_core::TaskExecutionTarget {
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    task_id: root_task_id.clone(),
                    requested_worker_id: None,
                    recovery_id: None,
                    execution_chain_ref: Some("chain-atomic-spawn".to_string()),
                },
                worker_id: WorkerId::new("worker-parent"),
                thread_id: ThreadId::new("thread-atomic-spawn-parent"),
                is_primary: true,
                session_id: session_id.clone(),
                workspace_id: workspace_id.clone(),
                ownership: ExecutionOwnership::default(),
                writebacks: ExecutionWritebackPlans::default(),
                use_tools: true,
                skill_name: None,
                images: Vec::new(),
                execution_settings_snapshot: Some(parent_settings_snapshot.clone()),
            },
        );

        let child = test_task("task-child", root_task_id.as_str(), &mission_id);
        let registered = registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                spawn_graph: &spawn_graph,
                session_store: &session_store,
                child_task: &child,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "executor",
                now,
            })
            .expect("spawned child runtime registration should succeed");

        assert_eq!(registered.execution_chain_ref, "chain-atomic-spawn");
        assert!(
            task_store.get_task(&child.task_id).is_some(),
            "child task should be inserted by the atomic runtime registration entry"
        );
        assert_eq!(
            spawn_graph
                .lock()
                .expect("spawn graph lock should be available")
                .parent_of(&child.task_id),
            Some(&root_task_id)
        );

        let plan = registry
            .get(&child.task_id)
            .expect("child execution plan should be registered atomically");
        match plan {
            TaskExecutionPlan::Dispatch {
                thread_id,
                session_id: plan_session_id,
                workspace_id: plan_workspace_id,
                is_primary,
                use_tools,
                execution_settings_snapshot,
                ..
            } => {
                assert_eq!(thread_id, registered.thread_id);
                assert_eq!(plan_session_id, session_id);
                assert_eq!(plan_workspace_id, workspace_id);
                assert!(!is_primary);
                assert!(use_tools);
                assert!(
                    execution_settings_snapshot
                        .as_ref()
                        .is_some_and(|snapshot| Arc::ptr_eq(snapshot, &parent_settings_snapshot)),
                    "agent_spawn 子任务必须继承父任务执行快照"
                );
            }
        }

        let chain = session_store
            .active_execution_chain(&session_id)
            .expect("active execution chain should remain available");
        let child_branch = chain
            .branches
            .iter()
            .find(|branch| {
                branch.task_id == child.task_id && branch.worker_id == registered.worker_id
            })
            .expect("child branch should exist");
        assert_eq!(child_branch.thread_id, registered.thread_id);
        assert!(chain.active_branch_task_ids.contains(&child.task_id));
        assert!(chain.active_worker_bindings.contains(&registered.worker_id));
    }
}
