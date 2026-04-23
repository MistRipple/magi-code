use crate::task_store::TaskStore;
use crate::{
    AssignmentGovernanceSummary, AssignmentSkillDispatchSummary, ExecutionContextSummary,
    ExecutionOverview, ExecutionRuntimeSnapshot, ExecutionSkillDispatchSummary,
    TaskGovernanceSummary, TaskSkillDispatchSummary,
};
use magi_core::{AssignmentId, MissionId, Task, TaskId, TaskProjection};
use magi_skill_runtime::{SkillDispatchRoute, SkillDispatchStatus};
use magi_tool_runtime::ToolExecutionSummary;
use magi_worker_runtime::{
    WorkerGovernanceObservation, WorkerGovernanceSummary, WorkerRuntimeSummary,
    WorkerSkillDispatchObservation,
};
use std::collections::HashSet;

pub(crate) fn build_execution_overview_payload(overview: &ExecutionOverview) -> serde_json::Value {
    let context_payload = overview
        .context_summary
        .as_ref()
        .map(|summary| {
            serde_json::to_value(summary).expect("execution context summary to serialize")
        })
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
        "mission_id": overview.runtime_snapshot.mission_id.to_string(),
        "total_tasks": overview.runtime_snapshot.total_tasks,
        "completed_tasks": overview.runtime_snapshot.completed_tasks,
        "failed_tasks": overview.runtime_snapshot.failed_tasks,
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

pub(crate) fn build_runtime_snapshot_from_projection(
    projection: &TaskProjection,
    total_assignments: usize,
) -> ExecutionRuntimeSnapshot {
    ExecutionRuntimeSnapshot {
        mission_id: projection.root_task.mission_id.clone(),
        total_assignments,
        total_tasks: projection.progress_summary.total_tasks as usize,
        completed_tasks: projection.progress_summary.completed_tasks as usize,
        failed_tasks: (projection.progress_summary.failed_tasks
            + projection.progress_summary.blocked_tasks) as usize,
    }
}

pub(crate) fn build_execution_overview_from_task_graph(
    task_store: &TaskStore,
    root_task_id: &TaskId,
    _mission_id: &MissionId,
    worker_summary: WorkerRuntimeSummary,
    tool_summary: ToolExecutionSummary,
    skill_dispatch_observations: &[WorkerSkillDispatchObservation],
    governance_observations: &[WorkerGovernanceObservation],
    context_summary: Option<ExecutionContextSummary>,
) -> Option<ExecutionOverview> {
    let projection = task_store.build_projection(root_task_id)?;
    let subtree_tasks = collect_subtree_tasks(task_store, root_task_id);
    let assignment_roots = collect_assignment_roots(task_store, &projection.root_task);
    let assignment_governance_summaries = build_assignment_governance_summaries_from_tasks(
        task_store,
        &projection.root_task,
        &assignment_roots,
        governance_observations,
    );
    let task_governance_summaries = build_task_governance_summaries_from_tasks(
        task_store,
        &projection.root_task,
        &subtree_tasks,
        governance_observations,
    );
    let assignment_skill_dispatch_summaries = build_assignment_skill_dispatch_summaries_from_tasks(
        task_store,
        &projection.root_task,
        &assignment_roots,
        skill_dispatch_observations,
    );
    let task_skill_dispatch_summaries = build_task_skill_dispatch_summaries_from_tasks(
        task_store,
        &projection.root_task,
        &subtree_tasks,
        skill_dispatch_observations,
    );

    Some(ExecutionOverview {
        runtime_snapshot: build_runtime_snapshot_from_projection(
            &projection,
            assignment_roots.len(),
        ),
        running_task_ids: projection.running_tasks.clone(),
        worker_summary,
        tool_summary,
        governance_summary: WorkerGovernanceSummary::from_observations(
            governance_observations.iter(),
        ),
        skill_dispatch_summary: build_skill_dispatch_summary(skill_dispatch_observations.iter()),
        context_summary,
        assignment_governance_summaries,
        task_governance_summaries,
        assignment_skill_dispatch_summaries,
        task_skill_dispatch_summaries,
    })
}

pub(crate) fn assignment_id_for_task(
    task_store: &TaskStore,
    root_task_id: &TaskId,
    task_id: &TaskId,
) -> Option<AssignmentId> {
    assignment_root_task(task_store, root_task_id, task_id)
        .map(|task| AssignmentId::new(task.task_id.as_str()))
}

pub(crate) fn assignment_title_for_task(
    task_store: &TaskStore,
    root_task_id: &TaskId,
    task_id: &TaskId,
) -> Option<String> {
    assignment_root_task(task_store, root_task_id, task_id).map(|task| task.title)
}

fn collect_subtree_tasks(task_store: &TaskStore, root_task_id: &TaskId) -> Vec<Task> {
    task_store
        .collect_subtree_ids(root_task_id)
        .into_iter()
        .filter_map(|task_id| task_store.get_task(&task_id))
        .collect()
}

fn collect_assignment_roots(task_store: &TaskStore, root_task: &Task) -> Vec<Task> {
    task_store.get_children(&root_task.task_id)
}

fn assignment_root_task(
    task_store: &TaskStore,
    root_task_id: &TaskId,
    task_id: &TaskId,
) -> Option<Task> {
    let mut current = task_store.get_task(task_id)?;
    if current.task_id == *root_task_id {
        return None;
    }
    loop {
        if current.parent_task_id.as_ref() == Some(root_task_id) {
            return Some(current);
        }
        let parent_id = current.parent_task_id.clone()?;
        current = task_store.get_task(&parent_id)?;
    }
}

fn build_assignment_governance_summaries_from_tasks(
    task_store: &TaskStore,
    root_task: &Task,
    assignment_roots: &[Task],
    observations: &[WorkerGovernanceObservation],
) -> Vec<AssignmentGovernanceSummary> {
    assignment_roots
        .iter()
        .map(|assignment_root| {
            let task_ids = task_store
                .collect_subtree_ids(&assignment_root.task_id)
                .into_iter()
                .collect::<HashSet<_>>();
            let filtered = observations.iter().filter(|observation| {
                observation
                    .task_id
                    .as_ref()
                    .is_some_and(|task_id| task_ids.contains(task_id))
            });
            AssignmentGovernanceSummary {
                assignment_id: AssignmentId::new(assignment_root.task_id.as_str()),
                mission_id: root_task.mission_id.clone(),
                governance_summary: build_governance_summary(filtered),
            }
        })
        .collect()
}

fn build_task_governance_summaries_from_tasks(
    task_store: &TaskStore,
    root_task: &Task,
    tasks: &[Task],
    observations: &[WorkerGovernanceObservation],
) -> Vec<TaskGovernanceSummary> {
    tasks
        .iter()
        .filter(|task| task.task_id != root_task.task_id)
        .map(|task| {
            let filtered = observations
                .iter()
                .filter(|observation| observation.task_id.as_ref() == Some(&task.task_id));
            TaskGovernanceSummary {
                task_id: task.task_id.clone(),
                mission_id: root_task.mission_id.clone(),
                assignment_id: assignment_id_for_task(
                    task_store,
                    &root_task.task_id,
                    &task.task_id,
                )
                .unwrap_or_else(|| AssignmentId::new(task.task_id.as_str())),
                governance_summary: build_governance_summary(filtered),
            }
        })
        .collect()
}

fn build_assignment_skill_dispatch_summaries_from_tasks(
    task_store: &TaskStore,
    root_task: &Task,
    assignment_roots: &[Task],
    observations: &[WorkerSkillDispatchObservation],
) -> Vec<AssignmentSkillDispatchSummary> {
    assignment_roots
        .iter()
        .map(|assignment_root| {
            let task_ids = task_store
                .collect_subtree_ids(&assignment_root.task_id)
                .into_iter()
                .collect::<HashSet<_>>();
            let filtered = observations
                .iter()
                .filter(|observation| task_ids.contains(&observation.task_id));
            AssignmentSkillDispatchSummary {
                assignment_id: AssignmentId::new(assignment_root.task_id.as_str()),
                mission_id: root_task.mission_id.clone(),
                skill_dispatch_summary: build_skill_dispatch_summary(filtered),
            }
        })
        .collect()
}

fn build_task_skill_dispatch_summaries_from_tasks(
    task_store: &TaskStore,
    root_task: &Task,
    tasks: &[Task],
    observations: &[WorkerSkillDispatchObservation],
) -> Vec<TaskSkillDispatchSummary> {
    tasks
        .iter()
        .filter(|task| task.task_id != root_task.task_id)
        .map(|task| {
            let filtered = observations
                .iter()
                .filter(|observation| observation.task_id == task.task_id);
            TaskSkillDispatchSummary {
                task_id: task.task_id.clone(),
                mission_id: root_task.mission_id.clone(),
                assignment_id: assignment_id_for_task(
                    task_store,
                    &root_task.task_id,
                    &task.task_id,
                )
                .unwrap_or_else(|| AssignmentId::new(task.task_id.as_str())),
                skill_dispatch_summary: build_skill_dispatch_summary(filtered),
            }
        })
        .collect()
}

fn build_skill_dispatch_summary<'a, I>(observations: I) -> ExecutionSkillDispatchSummary
where
    I: IntoIterator<Item = &'a WorkerSkillDispatchObservation>,
{
    let mut summary = ExecutionSkillDispatchSummary::default();
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
