//! Task System v2 — M15：session-thread 复用入口从
//! magi-api/dispatch_execution.rs 下沉到 conversation-runtime。
//!
//! `ensure_thread_for_role` 不再依赖 `&ApiState`，改为接收显式
//! `&SessionStore`。dispatch_execution.rs 通过 `pub use` 重导出，
//! 调用点保持不变；待 M16/M17 解除 magi-api 对 TaskExecutionPlan
//! 的所有权后，dispatch_execution.rs 可整体删除。

use magi_core::{MissionId, SessionId, TaskId, ThreadId, UtcMillis, WorkerId};
use magi_session_store::{ExecutionThread, ExecutionThreadStatus, SessionStore};

/// 为指定 role 获取可复用的 Thread：命中 Idle thread 则直接复用，否则在 registry
/// 中 spawn 一条新 Thread 并返回。调用后 thread 被标记为 Active，绑定当前 task。
///
/// P6a 与既有 lane 机制并存：lane 构造点拿到 thread_id 后仅作为信息挂载，不影响
/// worker 执行；P6b 才让 task_llm_loop 按 thread_id 载入历史 messages。
pub fn ensure_thread_for_role(
    session_store: &SessionStore,
    session_id: &SessionId,
    mission_id: &MissionId,
    role_id: &str,
    worker_instance_id: &WorkerId,
    task_id: &TaskId,
    now: UtcMillis,
) -> ThreadId {
    if let Some(existing) = session_store.find_idle_thread_for_role(session_id, role_id) {
        session_store.activate_thread(&existing.thread_id, task_id, now);
        return existing.thread_id;
    }
    let new_thread = ExecutionThread {
        thread_id: ThreadId::new(format!("thread-{role_id}-{}", now.0)),
        session_id: session_id.clone(),
        mission_id: mission_id.clone(),
        role_id: role_id.to_string(),
        worker_instance_id: worker_instance_id.clone(),
        status: ExecutionThreadStatus::Active,
        created_at: now,
        last_used_at: now,
        handled_task_ids: vec![task_id.clone()],
        message_history: Vec::new(),
    };
    let thread_id = new_thread.thread_id.clone();
    session_store.register_thread(new_thread);
    thread_id
}
