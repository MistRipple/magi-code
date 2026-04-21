
use crate::{MissionRecord, ResumeCommand};
use magi_core::{
    AssignmentId, DispatchReason, RecoveryResumeInput, ResumeDispatchDecision,
    TaskId, TaskStatus,
};

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

pub(crate) fn build_resume_dispatch_decision(
    mission: &MissionRecord,
    input: &RecoveryResumeInput,
) -> Option<ResumeDispatchDecision> {
    let mission_id = input.ownership.mission_id.as_ref()?;
    let (assignment_id, task_id) = if let Some(task_id) = input.ownership.task_id.as_ref() {
        let assignment = mission
            .assignments
            .iter()
            .find(|assignment| assignment.tasks.iter().any(|task| &task.task_id == task_id))?;
        (assignment.assignment_id.clone(), task_id.clone())
    } else {
        select_resume_target(mission)?
    };

    Some(ResumeDispatchDecision {
        mission_id: mission_id.clone(),
        assignment_id,
        task_id,
        worker_id: input.ownership.worker_id.clone(),
        dispatch_reason: DispatchReason::ManualResume,
        recovery_id: input.recovery_id.clone(),
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

pub(crate) fn build_resume_dispatch_payload(
    decision: &ResumeDispatchDecision,
) -> serde_json::Value {
    serde_json::json!({
        "mission_id": decision.mission_id.to_string(),
        "assignment_id": decision.assignment_id.to_string(),
        "task_id": decision.task_id.to_string(),
        "worker_id": decision.worker_id.as_ref().map(ToString::to_string),
        "recovery_id": decision.recovery_id,
        "dispatch_reason": format!("{:?}", decision.dispatch_reason),
        "execution_chain_ref": decision.execution_chain_ref
    })
}

pub(crate) fn build_resume_outcome_payload(
    decision: &ResumeDispatchDecision,
) -> serde_json::Value {
    build_resume_dispatch_payload(decision)
}

fn select_resume_target(mission: &MissionRecord) -> Option<(AssignmentId, TaskId)> {
    let preferred_statuses = [
        TaskStatus::Blocked,
        TaskStatus::Running,
        TaskStatus::Ready,
    ];
    for status in preferred_statuses {
        for assignment in &mission.assignments {
            if let Some(task) = assignment.tasks.iter().find(|task| task.status == status) {
                return Some((assignment.assignment_id.clone(), task.task_id.clone()));
            }
        }
    }
    None
}
