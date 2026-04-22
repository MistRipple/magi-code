use super::{
    collect_unique_option_string, AssignmentRuntimeSummaryEntry,
    ExecutionGroupRuntimeSummaryEntry,
    RecoveryActivityStage, RecoveryReadModelInput, RecoveryResumeObservationSummary,
    RuntimeAttentionSummary, RuntimeDiagnosticSummary, RuntimeWorkQueueSummary,
    TaskRuntimeSummaryEntry, ToolRuntimeSummaryEntry, WorkerRuntimeSummaryEntry,
};

impl RuntimeDiagnosticSummary {
    pub(super) fn from_components(
        execution_groups: &[ExecutionGroupRuntimeSummaryEntry],
        tasks: &[TaskRuntimeSummaryEntry],
        assignments: &[AssignmentRuntimeSummaryEntry],
        workers: &[WorkerRuntimeSummaryEntry],
        tools: &[ToolRuntimeSummaryEntry],
        recovery: &RecoveryReadModelInput,
        governance_total_count: usize,
        governance_allowed_count: usize,
        governance_needs_approval_count: usize,
        governance_blocked_count: usize,
        governance_rejected_count: usize,
    ) -> Self {
        Self {
            running_execution_group_count: count_status(
                execution_groups
                    .iter()
                    .map(|entry| entry.current_status.as_deref()),
                "running",
            ) + count_status(
                execution_groups
                    .iter()
                    .map(|entry| entry.current_status.as_deref()),
                "resuming",
            ),
            failed_execution_group_count: count_status(
                execution_groups
                    .iter()
                    .map(|entry| entry.current_status.as_deref()),
                "failed",
            ),
            running_task_count: count_status(
                tasks.iter().map(|entry| entry.current_status.as_deref()),
                "running",
            ),
            failed_task_count: count_status(
                tasks.iter().map(|entry| entry.current_status.as_deref()),
                "failed",
            ) + count_status(
                tasks.iter().map(|entry| entry.current_status.as_deref()),
                "blocked",
            ),
            running_assignment_count: count_status(
                assignments
                    .iter()
                    .map(|entry| entry.current_status.as_deref()),
                "running",
            ),
            failed_assignment_count: count_status(
                assignments
                    .iter()
                    .map(|entry| entry.current_status.as_deref()),
                "failed",
            ),
            active_worker_count: count_status(
                workers
                    .iter()
                    .map(|entry| entry.current_status.as_deref()),
                "running",
            ) + count_status(
                workers
                    .iter()
                    .map(|entry| entry.current_status.as_deref()),
                "reviewing",
            ) + count_status(
                workers
                    .iter()
                    .map(|entry| entry.current_status.as_deref()),
                "verifying",
            ) + count_status(
                workers
                    .iter()
                    .map(|entry| entry.current_status.as_deref()),
                "repairing",
            ),
            failed_worker_count: count_status(
                workers
                    .iter()
                    .map(|entry| entry.current_status.as_deref()),
                "failed",
            ),
            blocked_tool_count: tools.iter().map(|entry| entry.blocked_count).sum(),
            failed_tool_count: tools.iter().map(|entry| entry.failed_count).sum(),
            governance_total_count,
            governance_allowed_count,
            governance_needs_approval_count,
            governance_blocked_count,
            governance_rejected_count,
            rejected_skill_dispatch_count: workers
                .iter()
                .map(|entry| entry.rejected_dispatch_count)
                .sum(),
            failed_skill_dispatch_count: workers
                .iter()
                .map(|entry| entry.failed_dispatch_count)
                .sum(),
            context_execution_group_count: execution_groups
                .iter()
                .filter(|entry| {
                    entry.context_used_knowledge_count > 0
                        || entry.context_used_memory_count > 0
                        || entry.context_used_turn_count > 0
                        || entry.context_used_shared_item_count > 0
                        || entry.context_used_file_summary_count > 0
                })
                .count(),
            context_used_knowledge_count: execution_groups
                .iter()
                .map(|entry| entry.context_used_knowledge_count)
                .sum(),
            context_used_memory_count: execution_groups
                .iter()
                .map(|entry| entry.context_used_memory_count)
                .sum(),
            context_code_index_knowledge_count: execution_groups
                .iter()
                .map(|entry| entry.context_code_index_knowledge_count)
                .sum(),
            context_extracted_memory_count: execution_groups
                .iter()
                .map(|entry| entry.context_extracted_memory_count)
                .sum(),
            degraded_executor_count: workers
                .iter()
                .filter(|entry| entry.executor_observation_status.as_deref() == Some("degraded"))
                .count(),
            unavailable_executor_count: workers
                .iter()
                .filter(|entry| {
                    matches!(
                        entry.executor_observation_status.as_deref(),
                        Some("unavailable")
                    )
                })
                .count(),
            pending_recovery_count: recovery
                .summaries
                .iter()
                .filter(|entry| entry.current_status == "resuming")
                .count(),
            resumed_recovery_count: recovery
                .summaries
                .iter()
                .filter(|entry| {
                    entry.current_status == "mission_resumed"
                        || entry.current_status == "worker_resumed"
                })
                .count(),
        }
    }
}

impl RuntimeAttentionSummary {
    pub(super) fn from_components(
        execution_groups: &[ExecutionGroupRuntimeSummaryEntry],
        tasks: &[TaskRuntimeSummaryEntry],
        assignments: &[AssignmentRuntimeSummaryEntry],
        workers: &[WorkerRuntimeSummaryEntry],
        tools: &[ToolRuntimeSummaryEntry],
        recovery: &RecoveryReadModelInput,
        governance_blocked_task_ids: &[String],
        governance_approval_required_task_ids: &[String],
        governance_rejected_task_ids: &[String],
        governance_blocked_worker_ids: &[String],
        governance_approval_required_worker_ids: &[String],
        governance_rejected_worker_ids: &[String],
    ) -> Self {
        Self {
            failed_execution_group_ids: execution_groups
                .iter()
                .filter(|entry| entry.current_status.as_deref() == Some("failed"))
                .map(|entry| entry.mission_id.clone())
                .collect(),
            failed_task_ids: tasks
                .iter()
                .filter(|entry| {
                    matches!(
                        entry.current_status.as_deref(),
                        Some("failed") | Some("blocked")
                    )
                })
                .map(|entry| entry.task_id.clone())
                .collect(),
            failed_assignment_ids: assignments
                .iter()
                .filter(|entry| entry.current_status.as_deref() == Some("failed"))
                .map(|entry| entry.assignment_id.clone())
                .collect(),
            failed_worker_ids: workers
                .iter()
                .filter(|entry| entry.current_status.as_deref() == Some("failed"))
                .map(|entry| entry.worker_id.clone())
                .collect(),
            blocked_tool_names: tools
                .iter()
                .filter(|entry| entry.blocked_count > 0)
                .map(|entry| entry.tool_name.clone())
                .collect(),
            governance_blocked_task_ids: governance_blocked_task_ids.to_vec(),
            governance_approval_required_task_ids: governance_approval_required_task_ids.to_vec(),
            governance_rejected_task_ids: governance_rejected_task_ids.to_vec(),
            governance_blocked_worker_ids: governance_blocked_worker_ids.to_vec(),
            governance_approval_required_worker_ids: governance_approval_required_worker_ids
                .to_vec(),
            governance_rejected_worker_ids: governance_rejected_worker_ids.to_vec(),
            rejected_skill_dispatch_worker_ids: workers
                .iter()
                .filter(|entry| entry.rejected_dispatch_count > 0)
                .map(|entry| entry.worker_id.clone())
                .collect(),
            failed_skill_dispatch_worker_ids: workers
                .iter()
                .filter(|entry| entry.failed_dispatch_count > 0)
                .map(|entry| entry.worker_id.clone())
                .collect(),
            degraded_executor_worker_ids: workers
                .iter()
                .filter(|entry| entry.executor_observation_status.as_deref() == Some("degraded"))
                .map(|entry| entry.worker_id.clone())
                .collect(),
            unavailable_executor_worker_ids: workers
                .iter()
                .filter(|entry| entry.executor_observation_status.as_deref() == Some("unavailable"))
                .map(|entry| entry.worker_id.clone())
                .collect(),
            pending_recovery_ids: recovery
                .summaries
                .iter()
                .filter(|entry| entry.current_status == "resuming")
                .map(|entry| entry.recovery_id.clone())
                .collect(),
        }
    }
}

impl RuntimeWorkQueueSummary {
    pub(super) fn from_components(
        execution_groups: &[ExecutionGroupRuntimeSummaryEntry],
        tasks: &[TaskRuntimeSummaryEntry],
        assignments: &[AssignmentRuntimeSummaryEntry],
        workers: &[WorkerRuntimeSummaryEntry],
        recovery: &RecoveryReadModelInput,
    ) -> Self {
        Self {
            running_execution_group_ids: execution_groups
                .iter()
                .filter(|entry| {
                    matches!(
                        entry.current_status.as_deref(),
                        Some("running") | Some("resuming")
                    )
                })
                .map(|entry| entry.mission_id.clone())
                .collect(),
            running_task_ids: tasks
                .iter()
                .filter(|entry| entry.current_status.as_deref() == Some("running"))
                .map(|entry| entry.task_id.clone())
                .collect(),
            running_assignment_ids: assignments
                .iter()
                .filter(|entry| entry.current_status.as_deref() == Some("running"))
                .map(|entry| entry.assignment_id.clone())
                .collect(),
            active_worker_ids: workers
                .iter()
                .filter(|entry| {
                    matches!(
                        entry.current_status.as_deref(),
                        Some("running")
                            | Some("reviewing")
                            | Some("verifying")
                            | Some("repairing")
                    )
                })
                .map(|entry| entry.worker_id.clone())
                .collect(),
            pending_recovery_ids: recovery
                .summaries
                .iter()
                .filter(|entry| entry.current_status == "resuming")
                .map(|entry| entry.recovery_id.clone())
                .collect(),
        }
    }
}

impl RecoveryResumeObservationSummary {
    pub(super) fn from_recovery(recovery: &RecoveryReadModelInput) -> Self {
        let mut affected_execution_group_ids = Vec::new();
        let mut affected_worker_ids = Vec::new();
        let mut resume_command_count = 0;
        let mut resume_dispatch_count = 0;
        let mut mission_resumed_count = 0;
        let mut worker_resumed_count = 0;

        for entry in &recovery.entries {
            match entry.stage {
                RecoveryActivityStage::ResumeCommandCreated => resume_command_count += 1,
                RecoveryActivityStage::ResumeDispatchCreated => resume_dispatch_count += 1,
                RecoveryActivityStage::MissionResumed => mission_resumed_count += 1,
                RecoveryActivityStage::WorkerResumed => worker_resumed_count += 1,
            }
            collect_unique_option_string(
                &mut affected_execution_group_ids,
                entry.mission_id.as_ref().map(ToString::to_string),
            );
            collect_unique_option_string(&mut affected_worker_ids, entry.worker_id.clone());
        }

        Self {
            total_recoveries: recovery.summaries.len(),
            resume_command_count,
            resume_dispatch_count,
            mission_resumed_count,
            worker_resumed_count,
            affected_execution_group_ids,
            affected_worker_ids,
        }
    }
}

fn count_status<'a>(values: impl Iterator<Item = Option<&'a str>>, expected: &str) -> usize {
    values.filter(|value| *value == Some(expected)).count()
}
