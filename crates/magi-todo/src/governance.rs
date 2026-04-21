use crate::types::{
    ApprovalStatus, TodoExecutionBlocker, TodoProjectionStatus, UnifiedTodo,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoExecutionGateBlocker {
    Status,
    Dependencies,
    Contracts,
    Approval,
}

impl From<TodoExecutionBlocker> for TodoExecutionGateBlocker {
    fn from(b: TodoExecutionBlocker) -> Self {
        match b {
            TodoExecutionBlocker::Dependencies => Self::Dependencies,
            TodoExecutionBlocker::Contracts => Self::Contracts,
            TodoExecutionBlocker::Approval => Self::Approval,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TodoExecutionChecks {
    pub dependencies_met: bool,
    pub contracts_met: bool,
    pub approval_met: bool,
}

#[derive(Clone, Debug)]
pub struct TodoExecutionGate {
    pub executable: bool,
    pub blocked_by: Option<TodoExecutionGateBlocker>,
    pub reason: Option<String>,
    pub awaiting_approval: bool,
}

pub fn is_todo_awaiting_approval(
    out_of_scope: bool,
    approval_status: Option<ApprovalStatus>,
    status: TodoProjectionStatus,
) -> bool {
    out_of_scope
        && approval_status == Some(ApprovalStatus::Pending)
        && !status.canonicalize().is_terminal()
}

pub fn derive_todo_execution_gate(
    todo: &UnifiedTodo,
    checks: &TodoExecutionChecks,
    honor_stored_blocker: bool,
) -> TodoExecutionGate {
    let projection: TodoProjectionStatus = todo.status.into();

    if !projection.is_execution_candidate() {
        return TodoExecutionGate {
            executable: false,
            blocked_by: Some(TodoExecutionGateBlocker::Status),
            reason: Some(format!("当前状态 {:?} 不可执行", todo.status)),
            awaiting_approval: false,
        };
    }

    if honor_stored_blocker {
        if let Some(blocker) = &todo.execution_blocker {
            let awaiting = *blocker == TodoExecutionBlocker::Approval
                && is_todo_awaiting_approval(
                    todo.out_of_scope,
                    todo.approval_status,
                    projection,
                );
            return TodoExecutionGate {
                executable: false,
                blocked_by: Some((*blocker).into()),
                reason: Some(
                    todo.blocked_reason
                        .clone()
                        .unwrap_or_else(|| "当前 Todo 已被阻塞".into()),
                ),
                awaiting_approval: awaiting,
            };
        }
    }

    if !checks.dependencies_met {
        return TodoExecutionGate {
            executable: false,
            blocked_by: Some(TodoExecutionGateBlocker::Dependencies),
            reason: Some("等待前置 Todo 完成".into()),
            awaiting_approval: false,
        };
    }

    if !checks.contracts_met {
        return TodoExecutionGate {
            executable: false,
            blocked_by: Some(TodoExecutionGateBlocker::Contracts),
            reason: Some(format!(
                "等待契约: {}",
                todo.required_contracts.join(", ")
            )),
            awaiting_approval: false,
        };
    }

    if !checks.approval_met {
        return TodoExecutionGate {
            executable: false,
            blocked_by: Some(TodoExecutionGateBlocker::Approval),
            reason: Some("等待超范围审批".into()),
            awaiting_approval: is_todo_awaiting_approval(
                todo.out_of_scope,
                todo.approval_status,
                projection,
            ),
        };
    }

    TodoExecutionGate {
        executable: true,
        blocked_by: None,
        reason: None,
        awaiting_approval: false,
    }
}
