
use crate::{
    AssignmentGovernanceSummary, AssignmentSkillDispatchSummary, MissionContextSummary,
    MissionExecutionOverview, MissionRecord, MissionRuntimeSnapshot, MissionSkillDispatchSummary,
    TaskGovernanceSummary, TaskSkillDispatchSummary,
};
use magi_core::TaskStatus;
use magi_skill_runtime::{SkillDispatchRoute, SkillDispatchStatus};
use magi_tool_runtime::ToolExecutionSummary;
use magi_worker_runtime::{
    WorkerGovernanceObservation, WorkerGovernanceSummary, WorkerRuntimeSummary,
    WorkerSkillDispatchObservation,
};
use std::collections::HashSet;

pub(crate) fn build_execution_overview(
    mission: &MissionRecord,
    worker_summary: WorkerRuntimeSummary,
    tool_summary: ToolExecutionSummary,
    skill_dispatch_observations: &[WorkerSkillDispatchObservation],
    governance_observations: &[WorkerGovernanceObservation],
    context_summary: Option<MissionContextSummary>,
) -> MissionExecutionOverview {
    MissionExecutionOverview {
        mission: build_runtime_snapshot(mission),
        running_task_ids: mission
            .assignments
            .iter()
            .flat_map(|assignment| assignment.tasks.iter())
            .filter(|task| task.status == TaskStatus::Running)
            .map(|task| task.task_id.clone())
            .collect(),
        worker_summary,
        tool_summary,
        governance_summary: WorkerGovernanceSummary::from_observations(
            governance_observations.iter(),
        ),
        skill_dispatch_summary: build_skill_dispatch_summary(skill_dispatch_observations.iter()),
        context_summary,
        assignment_governance_summaries: build_assignment_governance_summaries(
            mission,
            governance_observations,
        ),
        task_governance_summaries: build_task_governance_summaries(mission, governance_observations),
        assignment_skill_dispatch_summaries: build_assignment_skill_dispatch_summaries(
            mission,
            skill_dispatch_observations,
        ),
        task_skill_dispatch_summaries: build_task_skill_dispatch_summaries(
            mission,
            skill_dispatch_observations,
        ),
    }
}

pub(crate) fn build_execution_overview_payload(
    overview: &MissionExecutionOverview,
) -> serde_json::Value {
    let context_payload = overview
        .context_summary
        .as_ref()
        .map(|summary| serde_json::to_value(summary).expect("mission context summary to serialize"))
        .unwrap_or(serde_json::Value::Null);
    let assignment_governance_payloads = overview
        .assignment_governance_summaries
        .iter()
        .map(|summary| {
            serde_json::json!({
                "assignment_id": summary.assignment_id.to_string(),
                "mission_id": summary.mission_id.to_string(),
                "governance_total": summary.governance_summary.total_checks,
                "governance_allowed": summary.governance_summary.allowed,
                "governance_needs_approval": summary.governance_summary.needs_approval,
                "governance_rejected": summary.governance_summary.rejected,
                "governance_blocked": summary.governance_summary.blocked,
                "governance_repair_retry": summary.governance_summary.repair_retry
            })
        })
        .collect::<Vec<_>>();
    let task_governance_payloads = overview
        .task_governance_summaries
        .iter()
        .map(|summary| {
            serde_json::json!({
                "task_id": summary.task_id.to_string(),
                "mission_id": summary.mission_id.to_string(),
                "assignment_id": summary.assignment_id.to_string(),
                "governance_total": summary.governance_summary.total_checks,
                "governance_allowed": summary.governance_summary.allowed,
                "governance_needs_approval": summary.governance_summary.needs_approval,
                "governance_rejected": summary.governance_summary.rejected,
                "governance_blocked": summary.governance_summary.blocked,
                "governance_repair_retry": summary.governance_summary.repair_retry
            })
        })
        .collect::<Vec<_>>();
    let assignment_skill_dispatch_payloads = overview
        .assignment_skill_dispatch_summaries
        .iter()
        .map(|summary| {
            serde_json::json!({
                "assignment_id": summary.assignment_id.to_string(),
                "mission_id": summary.mission_id.to_string(),
                "skill_dispatch_total": summary.skill_dispatch_summary.total_dispatches,
                "skill_dispatch_builtin": summary.skill_dispatch_summary.builtin_dispatches,
                "skill_dispatch_bridge": summary.skill_dispatch_summary.bridge_dispatches,
                "skill_dispatch_succeeded": summary.skill_dispatch_summary.succeeded_dispatches,
                "skill_dispatch_rejected": summary.skill_dispatch_summary.rejected_dispatches,
                "skill_dispatch_failed": summary.skill_dispatch_summary.failed_dispatches
            })
        })
        .collect::<Vec<_>>();
    let task_skill_dispatch_payloads = overview
        .task_skill_dispatch_summaries
        .iter()
        .map(|summary| {
            serde_json::json!({
                "task_id": summary.task_id.to_string(),
                "mission_id": summary.mission_id.to_string(),
                "assignment_id": summary.assignment_id.to_string(),
                "skill_dispatch_total": summary.skill_dispatch_summary.total_dispatches,
                "skill_dispatch_builtin": summary.skill_dispatch_summary.builtin_dispatches,
                "skill_dispatch_bridge": summary.skill_dispatch_summary.bridge_dispatches,
                "skill_dispatch_succeeded": summary.skill_dispatch_summary.succeeded_dispatches,
                "skill_dispatch_rejected": summary.skill_dispatch_summary.rejected_dispatches,
                "skill_dispatch_failed": summary.skill_dispatch_summary.failed_dispatches
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "mission_id": overview.mission.mission_id.to_string(),
        "total_tasks": overview.mission.total_tasks,
        "completed_tasks": overview.mission.completed_tasks,
        "failed_tasks": overview.mission.failed_tasks,
        "running_task_ids": overview.running_task_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "worker_total": overview.worker_summary.total_workers,
        "worker_active": overview.worker_summary.active_workers,
        "worker_reports": overview.worker_summary.report_count,
        "worker_tool_calls": overview.worker_summary.tool_call_count,
        "worker_skill_dispatches": overview.worker_summary.skill_dispatch_count,
        "worker_governance_checks": overview.worker_summary.governance_count,
        "worker_governance_allowed": overview.worker_summary.governance_summary.allowed,
        "worker_governance_needs_approval": overview.worker_summary.governance_summary.needs_approval,
        "worker_governance_rejected": overview.worker_summary.governance_summary.rejected,
        "worker_governance_blocked": overview.worker_summary.governance_summary.blocked,
        "worker_governance_repair_retry": overview.worker_summary.governance_summary.repair_retry,
        "worker_skill_dispatch_builtin": overview.worker_summary.skill_dispatch_summary.builtin_dispatches,
        "worker_skill_dispatch_bridge": overview.worker_summary.skill_dispatch_summary.bridge_dispatches,
        "worker_skill_dispatch_succeeded": overview.worker_summary.skill_dispatch_summary.succeeded_dispatches,
        "worker_skill_dispatch_rejected": overview.worker_summary.skill_dispatch_summary.rejected_dispatches,
        "worker_skill_dispatch_failed": overview.worker_summary.skill_dispatch_summary.failed_dispatches,
        "governance_total": overview.governance_summary.total_checks,
        "governance_allowed": overview.governance_summary.allowed,
        "governance_needs_approval": overview.governance_summary.needs_approval,
        "governance_rejected": overview.governance_summary.rejected,
        "governance_blocked": overview.governance_summary.blocked,
        "governance_repair_retry": overview.governance_summary.repair_retry,
        "assignment_governance_summaries": assignment_governance_payloads,
        "task_governance_summaries": task_governance_payloads,
        "tool_total": overview.tool_summary.total_invocations,
        "tool_success": overview.tool_summary.successful_invocations,
        "tool_blocked": overview.tool_summary.blocked_invocations,
        "tool_failed": overview.tool_summary.failed_invocations,
        "skill_dispatch_total": overview.skill_dispatch_summary.total_dispatches,
        "skill_dispatch_builtin": overview.skill_dispatch_summary.builtin_dispatches,
        "skill_dispatch_bridge": overview.skill_dispatch_summary.bridge_dispatches,
        "skill_dispatch_succeeded": overview.skill_dispatch_summary.succeeded_dispatches,
        "skill_dispatch_rejected": overview.skill_dispatch_summary.rejected_dispatches,
        "skill_dispatch_failed": overview.skill_dispatch_summary.failed_dispatches,
        "context": context_payload,
        "assignment_skill_dispatch_summaries": assignment_skill_dispatch_payloads,
        "task_skill_dispatch_summaries": task_skill_dispatch_payloads
    })
}

pub(crate) fn build_runtime_snapshot(mission: &MissionRecord) -> MissionRuntimeSnapshot {
    let total_assignments = mission.assignments.len();
    let all_tasks = mission
        .assignments
        .iter()
        .flat_map(|assignment| assignment.tasks.iter());
    let mut total_tasks = 0;
    let mut completed_tasks = 0;
    let mut failed_tasks = 0;
    for task in all_tasks {
        total_tasks += 1;
        if task.status == TaskStatus::Completed {
            completed_tasks += 1;
        }
        if matches!(
            task.status,
            TaskStatus::Failed | TaskStatus::Blocked
        ) {
            failed_tasks += 1;
        }
    }
    MissionRuntimeSnapshot {
        mission_id: mission.mission_id.clone(),
        total_assignments,
        total_tasks,
        completed_tasks,
        failed_tasks,
    }
}

fn build_assignment_skill_dispatch_summaries(
    mission: &MissionRecord,
    observations: &[WorkerSkillDispatchObservation],
) -> Vec<AssignmentSkillDispatchSummary> {
    mission
        .assignments
        .iter()
        .map(|assignment| {
            let task_ids = assignment
                .tasks
                .iter()
                .map(|task| task.task_id.clone())
                .collect::<HashSet<_>>();
            let filtered = observations
                .iter()
                .filter(|observation| task_ids.contains(&observation.task_id));
            AssignmentSkillDispatchSummary {
                assignment_id: assignment.assignment_id.clone(),
                mission_id: mission.mission_id.clone(),
                skill_dispatch_summary: build_skill_dispatch_summary(filtered),
            }
        })
        .collect()
}

fn build_assignment_governance_summaries(
    mission: &MissionRecord,
    observations: &[WorkerGovernanceObservation],
) -> Vec<AssignmentGovernanceSummary> {
    mission
        .assignments
        .iter()
        .map(|assignment| {
            let task_ids = assignment
                .tasks
                .iter()
                .map(|task| task.task_id.clone())
                .collect::<HashSet<_>>();
            let filtered = observations.iter().filter(|observation| {
                observation
                    .task_id
                    .as_ref()
                    .is_some_and(|task_id| task_ids.contains(task_id))
            });
            AssignmentGovernanceSummary {
                assignment_id: assignment.assignment_id.clone(),
                mission_id: mission.mission_id.clone(),
                governance_summary: build_governance_summary(filtered),
            }
        })
        .collect()
}

fn build_task_governance_summaries(
    mission: &MissionRecord,
    observations: &[WorkerGovernanceObservation],
) -> Vec<TaskGovernanceSummary> {
    mission
        .assignments
        .iter()
        .flat_map(|assignment| {
            assignment.tasks.iter().map(move |task| {
                let filtered = observations
                    .iter()
                    .filter(|observation| observation.task_id.as_ref() == Some(&task.task_id));
                TaskGovernanceSummary {
                    task_id: task.task_id.clone(),
                    mission_id: mission.mission_id.clone(),
                    assignment_id: assignment.assignment_id.clone(),
                    governance_summary: build_governance_summary(filtered),
                }
            })
        })
        .collect()
}

fn build_task_skill_dispatch_summaries(
    mission: &MissionRecord,
    observations: &[WorkerSkillDispatchObservation],
) -> Vec<TaskSkillDispatchSummary> {
    mission
        .assignments
        .iter()
        .flat_map(|assignment| {
            assignment.tasks.iter().map(move |task| {
                let filtered = observations
                    .iter()
                    .filter(|observation| observation.task_id == task.task_id);
                TaskSkillDispatchSummary {
                    task_id: task.task_id.clone(),
                    mission_id: mission.mission_id.clone(),
                    assignment_id: assignment.assignment_id.clone(),
                    skill_dispatch_summary: build_skill_dispatch_summary(filtered),
                }
            })
        })
        .collect()
}

fn build_skill_dispatch_summary<'a, I>(observations: I) -> MissionSkillDispatchSummary
where
    I: IntoIterator<Item = &'a WorkerSkillDispatchObservation>,
{
    let mut summary = MissionSkillDispatchSummary::default();
    for observation in observations {
        summary.total_dispatches += 1;
        match observation.route {
            Some(SkillDispatchRoute::Builtin) => summary.builtin_dispatches += 1,
            Some(SkillDispatchRoute::Bridge) => summary.bridge_dispatches += 1,
            None => {}
        }
        match observation.status {
            SkillDispatchStatus::Succeeded => summary.succeeded_dispatches += 1,
            SkillDispatchStatus::Rejected => summary.rejected_dispatches += 1,
            SkillDispatchStatus::Failed => summary.failed_dispatches += 1,
        }
    }
    summary
}

fn build_governance_summary<'a, I>(observations: I) -> WorkerGovernanceSummary
where
    I: IntoIterator<Item = &'a WorkerGovernanceObservation>,
{
    WorkerGovernanceSummary::from_observations(observations)
}
