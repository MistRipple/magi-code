//! 当前执行资格的只读检查；Task/lease、canonical Turn 与计划仍由原事实源拥有。
use crate::task_execution_registry::TaskExecutionRegistry;
use magi_core::{LeaseId, SessionId, TaskId};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::SessionStore;

pub(crate) struct TaskExecutionGuard<'a> {
    task_store: &'a TaskStore,
    session_store: &'a SessionStore,
    registry: &'a TaskExecutionRegistry,
    session: &'a SessionId,
    task: &'a TaskId,
    lease: &'a LeaseId,
    turn: Option<String>,
}

impl<'a> TaskExecutionGuard<'a> {
    pub(crate) fn new(
        task_store: &'a TaskStore,
        session_store: &'a SessionStore,
        registry: &'a TaskExecutionRegistry,
        session: &'a SessionId,
        task: &'a TaskId,
        lease: &'a LeaseId,
    ) -> Self {
        Self {
            task_store,
            session_store,
            registry,
            session,
            task,
            lease,
            turn: registry.turn_id(task),
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.task_store
            .execution_lease_is_current(self.task, self.lease)
            && match self.turn.as_deref() {
                Some(turn) => {
                    self.registry.turn_id(self.task).as_deref() == Some(turn)
                        && self
                            .session_store
                            .turn_execution_is_active(self.session, turn)
                }
                // 未绑定 canonical Turn 的轻量运行时仍须持有当前 Task/lease 与活跃父链。
                None => {
                    self.registry.turn_id(self.task).is_none()
                        && self
                            .task_store
                            .agent_run_transfer_for_task(self.task)
                            .is_ok_and(|transfer| transfer.is_none())
                }
            }
    }
}
