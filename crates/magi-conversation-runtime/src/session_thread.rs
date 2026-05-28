//! 任务系统 — session-thread 创建入口。
//!
//! `ensure_thread_for_role` 接收显式 `&SessionStore`，由 dispatch 装配层负责传入。

use magi_core::{MissionId, SessionId, TaskId, ThreadId, UtcMillis, WorkerId};
use magi_session_store::{ExecutionThread, ExecutionThreadStatus, SessionStore};

/// 为指定 role 创建本次 task 独占的 Thread，并返回其 thread_id。
///
/// worker 的执行事实必须由当前 task 驱动；旧 thread 只作为历史审计存在，不能被新
/// task 复用，否则 message_history 会把历史工具调用重新注入模型上下文。
pub fn ensure_thread_for_role(
    session_store: &SessionStore,
    session_id: &SessionId,
    mission_id: &MissionId,
    role_id: &str,
    worker_instance_id: &WorkerId,
    task_id: &TaskId,
    now: UtcMillis,
) -> ThreadId {
    let new_thread = ExecutionThread {
        thread_id: ThreadId::new(format!("thread-{role_id}-{}-{}", task_id.as_str(), now.0)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_thread_for_role_does_not_reuse_idle_role_thread() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-no-thread-reuse");
        let mission_id = MissionId::new("mission-no-thread-reuse");
        let role_id = "executor";
        let old_task_id = TaskId::new("task-old");
        let new_task_id = TaskId::new("task-new");
        let old_worker_id = WorkerId::new("worker-old");
        let new_worker_id = WorkerId::new("worker-new");

        store.register_thread(ExecutionThread {
            thread_id: ThreadId::new("thread-existing-idle"),
            session_id: session_id.clone(),
            mission_id: mission_id.clone(),
            role_id: role_id.to_string(),
            worker_instance_id: old_worker_id,
            status: ExecutionThreadStatus::Idle,
            created_at: UtcMillis(1_000),
            last_used_at: UtcMillis(1_000),
            handled_task_ids: vec![old_task_id],
            message_history: vec![magi_session_store::ThreadChatMessage {
                role: "user".to_string(),
                content: Some("历史任务：写 validation_auto_save_marker.txt".to_string()),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }],
        });

        let new_thread_id = ensure_thread_for_role(
            &store,
            &session_id,
            &mission_id,
            role_id,
            &new_worker_id,
            &new_task_id,
            UtcMillis(2_000),
        );

        assert_ne!(new_thread_id.as_str(), "thread-existing-idle");
        let threads = store.thread_registry_snapshot(&session_id);
        assert_eq!(threads.len(), 2);
        let new_thread = threads
            .iter()
            .find(|thread| thread.thread_id == new_thread_id)
            .expect("new task thread should be registered");
        assert_eq!(new_thread.handled_task_ids, vec![new_task_id]);
        assert!(
            new_thread.message_history.is_empty(),
            "新任务 thread 必须从空历史开始，不能继承旧 task 对话"
        );
    }
}
