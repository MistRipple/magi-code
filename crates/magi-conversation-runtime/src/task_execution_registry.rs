//! Task System v2 — M16：任务派发计划与注册中心从 magi-api/task_execution.rs
//! 下沉到 conversation-runtime。
//!
//! - [`TaskExecutionPlan`]：dispatch_submission 接受后挂在 task_execution_registry
//!   上的派发载体；M17/M18 接管派发触发后，本类型只保留 Dispatch 一支。
//! - [`TaskExecutionRegistry`]：线程安全的 `TaskId → TaskExecutionPlan` 索引，
//!   `LlmTaskDispatcher` 与 `Runner` 通过它取出已接受派发计划。
//!
//! magi-api 不再实现这两个类型，改为 `pub use` 重导出；待 LlmTaskDispatcher 在
//! M17/M18 整体下沉后，本模块仍是 v2 派发链路的唯一所有者。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use magi_core::{
    ExecutionOwnership, SessionId, TaskExecutionTarget, TaskId, ThreadId, WorkerId, WorkspaceId,
};
use magi_orchestrator::ExecutionWritebackPlans;

#[derive(Clone, Debug)]
pub enum TaskExecutionPlan {
    Dispatch {
        target: TaskExecutionTarget,
        worker_id: WorkerId,
        lane_id: Option<String>,
        lane_seq: Option<usize>,
        /// lane 绑定的 thread，由 `session_thread::ensure_thread_for_role` 创建或命中，
        /// 是 task_llm_loop 读取跨 task 历史消息的唯一路由键。
        thread_id: ThreadId,
        is_primary: bool,
        session_id: SessionId,
        workspace_id: Option<WorkspaceId>,
        ownership: ExecutionOwnership,
        writebacks: ExecutionWritebackPlans,
        use_tools: bool,
        skill_name: Option<String>,
    },
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
}
