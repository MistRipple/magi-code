
use crate::{ResumeCommand, task_store::TaskStore};
use magi_core::{DispatchReason, RecoveryResumeInput, TaskExecutionTarget, TaskKind, TaskStatus};

pub(crate) fn build_resume_command(input: &RecoveryResumeInput) -> Option<ResumeCommand> {
    let mission_id = input.ownership.mission_id.clone()?;
    Some(ResumeCommand {
        mission_id,
        task_id: input.ownership.task_id.clone(),
        dispatch_reason: DispatchReason::ManualResume,
        recovery_id: input.recovery_id.clone(),
        execution_chain_ref: input.ownership.execution_chain_ref.clone(),
    })
}

pub(crate) fn build_recovery_target(
    task_store: &TaskStore,
    input: &RecoveryResumeInput,
) -> Option<TaskExecutionTarget> {
    let mission_id = input.ownership.mission_id.clone()?;
    let root_task = task_store
        .get_tasks_by_mission(&mission_id)
        .into_iter()
        .find(|task| task.task_id == task.root_task_id)?;
    let executable_statuses = [TaskStatus::Blocked, TaskStatus::Running, TaskStatus::Ready];

    let task = if let Some(task_id) = input.ownership.task_id.as_ref() {
        let task = task_store.get_task(task_id)?;
        if task.root_task_id != root_task.task_id {
            return None;
        }
        if !is_recoverable_status(task.status) || !is_executable_task(&task.kind) {
            return None;
        }
        task
    } else {
        task_store
            .collect_subtree_ids(&root_task.task_id)
            .into_iter()
            .filter_map(|task_id| task_store.get_task(&task_id))
            .filter(|task| task.task_id != root_task.task_id)
            .filter(|task| is_executable_task(&task.kind))
            .find(|task| executable_statuses.contains(&task.status))?
    };

    Some(TaskExecutionTarget {
        mission_id,
        root_task_id: root_task.task_id,
        task_id: task.task_id,
        requested_worker_id: input.ownership.worker_id.clone(),
        recovery_id: Some(input.recovery_id.clone()),
        execution_chain_ref: input.ownership.execution_chain_ref.clone(),
    })
}

pub(crate) fn build_resume_command_payload(command: &ResumeCommand) -> serde_json::Value {
    serde_json::json!({
        "mission_id": command.mission_id.to_string(),
        "task_id": command.task_id.as_ref().map(ToString::to_string),
        "dispatch_reason": format!("{:?}", command.dispatch_reason),
        "recovery_id": command.recovery_id,
        "execution_chain_ref": command.execution_chain_ref
    })
}

fn is_recoverable_status(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Blocked | TaskStatus::Running | TaskStatus::Ready)
}

fn is_executable_task(kind: &TaskKind) -> bool {
    matches!(
        kind,
        TaskKind::Action | TaskKind::Validation | TaskKind::Repair | TaskKind::Decision
    )
}
