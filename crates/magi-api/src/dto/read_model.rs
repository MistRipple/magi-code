use magi_core::{
    MissionLifecyclePhase, TaskId, TaskStatus, public_runtime_summary, public_runtime_text,
};
use magi_event_bus::{
    ExecutionGroupRuntimeSummaryEntry, MissionMetricsSummary, RecoveryActivityStage,
    RecoveryDiagnosticSummaryEntry, RuntimeLedgerSummary, RuntimeReadModelInput,
    SessionRuntimeBranchSummaryEntry, SessionRuntimeSummaryEntry,
    SessionRuntimeTurnItemSummaryEntry, SessionRuntimeTurnSummaryEntry,
    WorkspaceRuntimeSummaryEntry,
};
use magi_mission_metrics::MissionMetrics;
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{SessionExecutionSidecarStatus, SessionRuntimeSidecarExport};
use magi_workspace::{RecoveryStatus, WorkspaceRecoverySidecarExport};
use std::collections::HashMap;

#[cfg(test)]
use magi_event_bus::AuditUsageLedgerStatus;

/// Mission §1.4 聚合根派生属性的快照,用于从 `AppState` 收集后注入读模型 DTO。
///
/// 反孤儿:`MissionAggregate::lifecycle_phase()` / `metrics()` 在 Phase A
/// 落地后必须有真实消费方,本结构就是 read-model 的入口。`mission_id` 用
/// 字符串形式与已有 `ExecutionGroupRuntimeSummaryEntry::mission_id` 对齐。
#[derive(Clone, Debug)]
pub struct MissionAggregateExport {
    pub mission_id: String,
    pub lifecycle_phase: MissionLifecyclePhase,
    pub metrics: Option<MissionMetrics>,
}

pub type RuntimeReadModelDto = RuntimeReadModelInput;
pub type AuditUsageLedgerDto = RuntimeLedgerSummary;

pub fn runtime_read_model_dto(
    runtime_read_model: RuntimeReadModelInput,
    session_sidecar_exports: &[SessionRuntimeSidecarExport],
    workspace_sidecar_exports: &[WorkspaceRecoverySidecarExport],
    audit_usage_ledger: AuditUsageLedgerDto,
    task_store: Option<&TaskStore>,
    mission_aggregate_exports: &[MissionAggregateExport],
) -> RuntimeReadModelDto {
    let mut runtime_read_model = runtime_read_model;
    merge_session_sidecars(&mut runtime_read_model, session_sidecar_exports, task_store);
    merge_workspace_sidecars(&mut runtime_read_model, workspace_sidecar_exports);
    merge_mission_aggregates(&mut runtime_read_model, mission_aggregate_exports);
    prune_terminal_runtime_live_ids(&mut runtime_read_model);
    runtime_read_model.meta.ledger = audit_usage_ledger;
    runtime_read_model
}

#[cfg(test)]
pub fn ledger_dto(status: AuditUsageLedgerStatus) -> AuditUsageLedgerDto {
    RuntimeLedgerSummary::from(status)
}

fn merge_session_sidecars(
    runtime_read_model: &mut RuntimeReadModelInput,
    session_sidecar_exports: &[SessionRuntimeSidecarExport],
    task_store: Option<&TaskStore>,
) {
    for export in session_sidecar_exports {
        let session_id = export.session_id.to_string();
        let entry = runtime_read_model
            .details
            .sessions
            .iter_mut()
            .find(|entry| entry.session_id == session_id);
        let entry = match entry {
            Some(entry) => entry,
            None => {
                runtime_read_model
                    .details
                    .sessions
                    .push(SessionRuntimeSummaryEntry {
                        session_id: session_id.clone(),
                        ..SessionRuntimeSummaryEntry::default()
                    });
                runtime_read_model
                    .details
                    .sessions
                    .last_mut()
                    .expect("session entry inserted above")
            }
        };

        entry.current_status = Some(match export.current_status {
            SessionExecutionSidecarStatus::Detached => "detached".to_string(),
            SessionExecutionSidecarStatus::Bound => "bound".to_string(),
            SessionExecutionSidecarStatus::RecoveryLinked => "recovery_linked".to_string(),
            SessionExecutionSidecarStatus::Resumed => "resumed".to_string(),
        });
        entry.last_update = Some(export.last_update);
        entry.execution_chain_ref = export.execution_chain_ref.clone();
        entry.recovery_ref = export.recovery_ref.clone();
        entry.active_execution_group_ids.clear();
        entry.active_task_ids.clear();
        entry.recovery_ids.clear();
        entry.active_branches.clear();
        entry.has_recoverable_chain = false;
        entry.recoverable_branch_count = 0;
        entry.mission_id = None;
        entry.root_task_id = None;
        entry.root_task_status = None;
        entry.root_task_created_at = None;
        entry.current_turn = None;
        entry.turn_items.clear();
        if let Some(chain) = export.active_execution_chain.as_ref() {
            entry.mission_id = Some(chain.mission_id.to_string());
            entry.root_task_id = Some(chain.root_task_id.to_string());
            entry.execution_chain_ref = Some(chain.execution_chain_ref.clone());
            entry.recovery_ref = chain.recovery_ref.clone().or(export.recovery_ref.clone());
            if let Some(task) = task_store.and_then(|store| store.get_task(&chain.root_task_id)) {
                entry.root_task_status = Some(task_status_label(&task.status));
                entry.root_task_created_at = Some(task.created_at);
            }
            entry.active_branches = chain
                .branches
                .iter()
                .map(|branch| session_branch_summary(branch, task_store))
                .collect();
            entry.recoverable_branch_count =
                if session_root_task_allows_continue(entry.root_task_status.as_deref()) {
                    entry
                        .active_branches
                        .iter()
                        .filter(|branch| branch_is_recoverable(branch))
                        .count()
                } else {
                    0
                };
            entry.has_recoverable_chain = entry.recoverable_branch_count > 0;
            push_unique(
                &mut entry.active_execution_group_ids,
                Some(chain.mission_id.to_string()),
            );
            for task_id in &chain.active_branch_task_ids {
                push_unique(&mut entry.active_task_ids, Some(task_id.to_string()));
            }
            push_unique(&mut entry.recovery_ids, chain.recovery_ref.clone());
        }
        if session_sidecar_is_runtime_active(export)
            && !session_root_task_is_terminal(entry.root_task_status.as_deref())
        {
            push_unique(
                &mut entry.active_execution_group_ids,
                export
                    .ownership
                    .mission_id
                    .as_ref()
                    .map(ToString::to_string),
            );
            push_unique(
                &mut entry.active_task_ids,
                export.ownership.task_id.as_ref().map(ToString::to_string),
            );
            push_unique(&mut entry.recovery_ids, export.recovery_ref.clone());
        }
        if session_root_task_is_terminal(entry.root_task_status.as_deref())
            && !entry.has_recoverable_chain
        {
            clear_session_runtime_live_ids(entry);
        }
        if let Some(turn) = export.current_turn.as_ref() {
            let completed_at = turn.completed_at;
            entry.current_turn = Some(SessionRuntimeTurnSummaryEntry {
                turn_id: turn.turn_id.clone(),
                turn_seq: turn.turn_seq,
                accepted_at: Some(turn.accepted_at),
                completed_at,
                response_duration_ms: completed_at
                    .map(|completed_at| completed_at.0.saturating_sub(turn.accepted_at.0)),
                status: turn.status.clone(),
                user_message: turn.user_message.clone(),
                mission_id: entry.mission_id.clone(),
                root_task_id: entry.root_task_id.clone(),
                execution_chain_ref: entry.execution_chain_ref.clone(),
            });
            entry.turn_items = turn
                .items
                .iter()
                .map(|item| {
                    let role_id = item.role_id.clone().or_else(|| {
                        item.task_id
                            .as_ref()
                            .and_then(|task_id| task_role_id(task_store, task_id))
                    });
                    SessionRuntimeTurnItemSummaryEntry {
                        item_id: item.item_id.clone(),
                        item_seq: item.item_seq,
                        kind: item.kind.clone(),
                        status: item.status.clone(),
                        source: item.source.clone(),
                        title: item.title.clone(),
                        content: item.content.clone(),
                        task_id: item.task_id.as_ref().map(ToString::to_string),
                        worker_id: item.worker_id.as_ref().map(ToString::to_string),
                        role_id,
                        tool_call_id: item.tool_call_id.clone(),
                        tool_name: item.tool_name.clone(),
                        tool_status: item.tool_status.clone(),
                        tool_arguments: public_runtime_turn_tool_text(&item.tool_arguments),
                        tool_result: public_runtime_turn_tool_text(&item.tool_result),
                        tool_error: public_runtime_turn_tool_text(&item.tool_error),
                        request_id: item.request_id.clone(),
                        user_message_id: item.user_message_id.clone(),
                        placeholder_message_id: item.placeholder_message_id.clone(),
                        timeline_entry_id: item.timeline_entry_id.clone(),
                        source_thread_id: item.source_thread_id.to_string(),
                    }
                })
                .collect();
        }
        if let Some(chain) = export.active_execution_chain.as_ref() {
            merge_task_store_projection(runtime_read_model, task_store, chain);
        }
    }

    runtime_read_model
        .details
        .sessions
        .sort_by(|left, right| left.session_id.cmp(&right.session_id));
    runtime_read_model
        .details
        .execution_groups
        .sort_by(|left, right| left.mission_id.cmp(&right.mission_id));
    runtime_read_model
        .details
        .tasks
        .sort_by(|left, right| left.task_id.cmp(&right.task_id));
    runtime_read_model
        .overview
        .activity
        .active_execution_group_ids
        .sort();
    runtime_read_model
        .overview
        .activity
        .active_execution_group_ids
        .dedup();
    runtime_read_model.overview.activity.active_task_ids.sort();
    runtime_read_model.overview.activity.active_task_ids.dedup();
    for entry in &mut runtime_read_model.details.execution_groups {
        entry.active_task_ids.sort();
        entry.active_task_ids.dedup();
    }
    for entry in &mut runtime_read_model.details.sessions {
        entry.active_execution_group_ids.sort();
        entry.active_execution_group_ids.dedup();
        entry.active_task_ids.sort();
        entry.active_task_ids.dedup();
        entry.recovery_ids.sort();
        entry.recovery_ids.dedup();
        entry.active_branches.sort_by(|left, right| {
            left.task_id
                .cmp(&right.task_id)
                .then_with(|| left.worker_id.cmp(&right.worker_id))
        });
        entry.turn_items.sort_by(|left, right| {
            left.item_seq
                .cmp(&right.item_seq)
                .then_with(|| left.item_id.cmp(&right.item_id))
        });
    }
}

fn task_role_id(task_store: Option<&TaskStore>, task_id: &TaskId) -> Option<String> {
    task_store
        .and_then(|store| store.get_task(task_id))
        .and_then(|task| task.executor_binding_target_role().map(str::to_string))
}

fn public_runtime_turn_tool_text(value: &Option<String>) -> Option<String> {
    let value = value.as_deref()?.trim();
    if value.is_empty() {
        return None;
    }
    let public = public_runtime_text(value);
    if public.is_empty() {
        None
    } else {
        Some(public)
    }
}

fn session_branch_summary(
    branch: &magi_session_store::ActiveExecutionBranch,
    task_store: Option<&TaskStore>,
) -> SessionRuntimeBranchSummaryEntry {
    let status = task_store
        .and_then(|store| store.get_task(&branch.task_id))
        .map(|task| task_status_label(&task.status))
        .unwrap_or_else(|| "unknown".to_string());
    SessionRuntimeBranchSummaryEntry {
        task_id: branch.task_id.to_string(),
        worker_id: branch.worker_id.to_string(),
        status,
        stage: branch.stage.clone(),
        lease_id: branch.lease_id.as_ref().map(ToString::to_string),
        execution_intent_ref: branch.execution_intent_ref.clone(),
        binding_lifecycle: branch.binding_lifecycle.clone(),
        checkpoint_stage: branch.checkpoint_stage.clone(),
        next_step_index: branch.next_step_index,
        checkpoint_at: branch.checkpoint_at,
        resume_mode: branch.resume_mode.clone(),
        is_primary: branch.is_primary,
    }
}

fn task_status_label(status: &TaskStatus) -> String {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Killed => "killed",
    }
    .to_string()
}

fn branch_stage_is_terminal(stage: &str) -> bool {
    matches!(
        stage.trim().to_ascii_lowercase().as_str(),
        "finish" | "finished"
    )
}

fn branch_is_recoverable(branch: &SessionRuntimeBranchSummaryEntry) -> bool {
    // 与 `magi_conversation_runtime::execution_chain_recovery::active_execution_branch_is_continue_recoverable`
    // 保持一致：UI 看到的可恢复语义必须等价于 `/api/session/continue` 实际接受的语义，
    // 避免出现 UI 与 API 对"可继续"判断不一致的两套实现。
    // 只有 `Failed` 且 stage 非 finish 的 branch 才是"可继续"。
    matches!(branch.status.as_str(), "failed") && !branch_stage_is_terminal(&branch.stage)
}

fn session_root_task_allows_continue(status: Option<&str>) -> bool {
    !matches!(status, Some("completed" | "killed"))
}

fn session_root_task_is_terminal(status: Option<&str>) -> bool {
    matches!(status, Some("completed" | "failed" | "killed"))
}

fn clear_session_runtime_live_ids(entry: &mut SessionRuntimeSummaryEntry) {
    entry.active_execution_group_ids.clear();
    entry.active_task_ids.clear();
    entry.has_recoverable_chain = false;
    entry.recoverable_branch_count = 0;
}

fn runtime_status_is_terminal(status: Option<&str>) -> bool {
    matches!(
        status,
        Some("completed" | "failed" | "killed" | "cancelled" | "canceled")
    )
}

fn keep_runtime_live_id(id: &str, status_by_id: &HashMap<String, String>) -> bool {
    !runtime_status_is_terminal(status_by_id.get(id).map(String::as_str))
}

fn prune_terminal_runtime_live_ids(runtime_read_model: &mut RuntimeReadModelInput) {
    let task_status_by_id = runtime_read_model
        .details
        .tasks
        .iter()
        .filter_map(|task| {
            task.current_status
                .as_ref()
                .map(|status| (task.task_id.clone(), status.clone()))
        })
        .collect::<HashMap<_, _>>();
    let group_status_by_id = runtime_read_model
        .details
        .execution_groups
        .iter()
        .filter_map(|group| {
            group
                .current_status
                .as_ref()
                .map(|status| (group.mission_id.clone(), status.clone()))
        })
        .collect::<HashMap<_, _>>();

    runtime_read_model
        .overview
        .activity
        .active_task_ids
        .retain(|task_id| keep_runtime_live_id(task_id, &task_status_by_id));
    runtime_read_model
        .overview
        .activity
        .active_execution_group_ids
        .retain(|mission_id| keep_runtime_live_id(mission_id, &group_status_by_id));

    for entry in &mut runtime_read_model.details.sessions {
        entry
            .active_task_ids
            .retain(|task_id| keep_runtime_live_id(task_id, &task_status_by_id));
        entry
            .active_execution_group_ids
            .retain(|mission_id| keep_runtime_live_id(mission_id, &group_status_by_id));
    }
    for entry in &mut runtime_read_model.details.workspaces {
        entry
            .active_task_ids
            .retain(|task_id| keep_runtime_live_id(task_id, &task_status_by_id));
        entry
            .active_execution_group_ids
            .retain(|mission_id| keep_runtime_live_id(mission_id, &group_status_by_id));
    }
    for entry in &mut runtime_read_model.details.execution_groups {
        entry
            .active_task_ids
            .retain(|task_id| keep_runtime_live_id(task_id, &task_status_by_id));
    }
}

fn session_sidecar_is_runtime_active(export: &SessionRuntimeSidecarExport) -> bool {
    matches!(
        export.current_status,
        SessionExecutionSidecarStatus::RecoveryLinked | SessionExecutionSidecarStatus::Resumed
    )
}

fn merge_workspace_sidecars(
    runtime_read_model: &mut RuntimeReadModelInput,
    workspace_sidecar_exports: &[WorkspaceRecoverySidecarExport],
) {
    for export in workspace_sidecar_exports {
        let workspace_id = export.workspace_id.to_string();
        let entry = runtime_read_model
            .details
            .workspaces
            .iter_mut()
            .find(|entry| entry.workspace_id == workspace_id);
        let entry = match entry {
            Some(entry) => entry,
            None => {
                runtime_read_model
                    .details
                    .workspaces
                    .push(WorkspaceRuntimeSummaryEntry {
                        workspace_id: workspace_id.clone(),
                        ..WorkspaceRuntimeSummaryEntry::default()
                    });
                runtime_read_model
                    .details
                    .workspaces
                    .last_mut()
                    .expect("workspace entry inserted above")
            }
        };

        entry.current_status = Some(match export.current_status {
            RecoveryStatus::Prepared => "prepared".to_string(),
            RecoveryStatus::Ready => "ready".to_string(),
            RecoveryStatus::Consumed => "consumed".to_string(),
        });
        entry.last_update = Some(export.last_update);
        entry.execution_chain_ref = export.execution_chain_ref.clone();
        entry.recovery_ref = Some(export.recovery_ref.clone());
        push_unique(
            &mut entry.active_execution_group_ids,
            export
                .ownership
                .mission_id
                .as_ref()
                .map(ToString::to_string),
        );
        push_unique(
            &mut entry.active_task_ids,
            export.ownership.task_id.as_ref().map(ToString::to_string),
        );
        push_unique(&mut entry.recovery_ids, Some(export.recovery_ref.clone()));
        push_unique(
            &mut entry.execution_chain_refs,
            export.execution_chain_ref.clone(),
        );

        let summary = runtime_read_model
            .recovery
            .summaries
            .iter_mut()
            .find(|summary| summary.recovery_id == export.recovery_ref);
        let public_diagnostic_summary =
            public_runtime_summary(export.diagnostic_summary.as_deref());
        let summary = match summary {
            Some(summary) => summary,
            None => {
                runtime_read_model
                    .recovery
                    .summaries
                    .push(RecoveryDiagnosticSummaryEntry {
                        recovery_id: export.recovery_ref.clone(),
                        event_count: 0,
                        latest_stage: recovery_stage_from_status(&export.current_status),
                        latest_event_type: "workspace.recovery.sidecar".to_string(),
                        latest_sequence: 0,
                        latest_occurred_at: export.last_update,
                        workspace_id: Some(export.workspace_id.clone()),
                        session_id: export.ownership.session_id.clone(),
                        mission_id: export.ownership.mission_id.clone(),
                        assignment_id: None,
                        task_id: export.ownership.task_id.clone(),
                        worker_id: export.ownership.worker_id.as_ref().map(ToString::to_string),
                        execution_chain_ref: export.execution_chain_ref.clone(),
                        diagnostic_summary: public_diagnostic_summary.clone(),
                        current_status: entry.current_status.clone().unwrap_or_default(),
                    });
                runtime_read_model
                    .recovery
                    .summaries
                    .last_mut()
                    .expect("recovery summary inserted above")
            }
        };
        if summary.workspace_id.is_none() {
            summary.workspace_id = Some(export.workspace_id.clone());
        }
        if summary.session_id.is_none() {
            summary.session_id = export.ownership.session_id.clone();
        }
        if summary.mission_id.is_none() {
            summary.mission_id = export.ownership.mission_id.clone();
        }
        if summary.task_id.is_none() {
            summary.task_id = export.ownership.task_id.clone();
        }
        if summary.worker_id.is_none() {
            summary.worker_id = export.ownership.worker_id.as_ref().map(ToString::to_string);
        }
        if summary.execution_chain_ref.is_none() {
            summary.execution_chain_ref = export.execution_chain_ref.clone();
        }
        if summary.diagnostic_summary.is_none() {
            summary.diagnostic_summary = public_diagnostic_summary;
        }
        if summary.event_count == 0 {
            summary.latest_occurred_at = export.last_update;
            summary.current_status = entry.current_status.clone().unwrap_or_default();
        }
        if !matches!(export.current_status, RecoveryStatus::Consumed) {
            push_unique(
                &mut runtime_read_model.recovery.active_recovery_ids,
                Some(export.recovery_ref.clone()),
            );
        }
    }

    runtime_read_model
        .details
        .workspaces
        .sort_by(|left, right| left.workspace_id.cmp(&right.workspace_id));
    runtime_read_model
        .recovery
        .summaries
        .sort_by(|left, right| left.recovery_id.cmp(&right.recovery_id));
    runtime_read_model.recovery.active_recovery_ids.sort();
    runtime_read_model.recovery.active_recovery_ids.dedup();
    for entry in &mut runtime_read_model.details.workspaces {
        entry.active_execution_group_ids.sort();
        entry.active_execution_group_ids.dedup();
        entry.active_task_ids.sort();
        entry.active_task_ids.dedup();
        entry.recovery_ids.sort();
        entry.recovery_ids.dedup();
        entry.execution_chain_refs.sort();
        entry.execution_chain_refs.dedup();
    }
}

fn merge_mission_aggregates(
    runtime_read_model: &mut RuntimeReadModelInput,
    exports: &[MissionAggregateExport],
) {
    for export in exports {
        let entry = runtime_read_model
            .details
            .execution_groups
            .iter_mut()
            .find(|entry| entry.mission_id == export.mission_id);
        let entry = match entry {
            Some(entry) => entry,
            None => {
                runtime_read_model.details.execution_groups.push(
                    ExecutionGroupRuntimeSummaryEntry {
                        mission_id: export.mission_id.clone(),
                        ..ExecutionGroupRuntimeSummaryEntry::default()
                    },
                );
                runtime_read_model
                    .details
                    .execution_groups
                    .last_mut()
                    .expect("execution group entry inserted above")
            }
        };
        entry.lifecycle_phase = Some(export.lifecycle_phase.as_str().to_string());
        entry.metrics = export.metrics.as_ref().map(metrics_to_summary);
    }
}

fn metrics_to_summary(metrics: &MissionMetrics) -> MissionMetricsSummary {
    MissionMetricsSummary {
        turn_count: metrics.turn_count,
        total_prompt_tokens: metrics.total_prompt_tokens,
        total_completion_tokens: metrics.total_completion_tokens,
        total_tokens: metrics.total_tokens,
        wall_clock_millis: metrics.wall_clock_millis,
        first_turn_started_at: metrics.first_turn_started_at,
        last_turn_finished_at: metrics.last_turn_finished_at,
        last_lifecycle_phase: metrics.last_lifecycle_phase.map(|p| p.as_str().to_string()),
    }
}

fn recovery_stage_from_status(status: &RecoveryStatus) -> RecoveryActivityStage {
    match status {
        RecoveryStatus::Prepared => RecoveryActivityStage::ResumeCommandCreated,
        RecoveryStatus::Ready => RecoveryActivityStage::ResumeDispatchCreated,
        RecoveryStatus::Consumed => RecoveryActivityStage::WorkerResumed,
    }
}

fn push_unique(values: &mut Vec<String>, value: Option<String>) {
    let Some(value) = value else {
        return;
    };
    if !values.contains(&value) {
        values.push(value);
    }
}

fn merge_task_store_projection(
    runtime_read_model: &mut RuntimeReadModelInput,
    task_store: Option<&TaskStore>,
    chain: &magi_session_store::ActiveExecutionChain,
) {
    let Some(projection) = task_store.and_then(|store| store.build_projection(&chain.root_task_id))
    else {
        return;
    };
    let mission_id = projection.root_task.mission_id.to_string();
    let active_task_ids = projection
        .tasks
        .iter()
        .filter(|task| !task_status_is_terminal(&task.status))
        .map(|task| task.task_id.to_string())
        .collect::<Vec<_>>();

    let group = runtime_read_model
        .details
        .execution_groups
        .iter_mut()
        .find(|entry| entry.mission_id == mission_id);
    let group = match group {
        Some(group) => group,
        None => {
            runtime_read_model
                .details
                .execution_groups
                .push(ExecutionGroupRuntimeSummaryEntry {
                    mission_id: mission_id.clone(),
                    ..ExecutionGroupRuntimeSummaryEntry::default()
                });
            runtime_read_model
                .details
                .execution_groups
                .last_mut()
                .expect("execution group entry inserted above")
        }
    };
    group.current_status = Some(task_status_label(&projection.root_task.status));
    group.active_task_ids.clear();
    for task_id in &active_task_ids {
        push_unique(&mut group.active_task_ids, Some(task_id.clone()));
    }

    if !task_status_is_terminal(&projection.root_task.status) {
        push_unique(
            &mut runtime_read_model
                .overview
                .activity
                .active_execution_group_ids,
            Some(mission_id.clone()),
        );
    }
    for task_id in &active_task_ids {
        push_unique(
            &mut runtime_read_model.overview.activity.active_task_ids,
            Some(task_id.clone()),
        );
    }

    for task in projection.tasks {
        let task_id = task.task_id.to_string();
        let task_entry = runtime_read_model
            .details
            .tasks
            .iter_mut()
            .find(|entry| entry.task_id == task_id);
        let task_entry = match task_entry {
            Some(task_entry) => task_entry,
            None => {
                runtime_read_model
                    .details
                    .tasks
                    .push(magi_event_bus::TaskRuntimeSummaryEntry {
                        task_id: task_id.clone(),
                        mission_id: Some(mission_id.clone()),
                        ..magi_event_bus::TaskRuntimeSummaryEntry::default()
                    });
                runtime_read_model
                    .details
                    .tasks
                    .last_mut()
                    .expect("task entry inserted above")
            }
        };
        task_entry.title = Some(task.title.clone());
        task_entry.mission_id = Some(mission_id.clone());
        task_entry.current_status = Some(task_status_label(&task.status));
    }
}

fn task_status_is_terminal(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{ExecutionOwnership, SessionId, ThreadId, UtcMillis, WorkspaceId};
    use magi_event_bus::RUNTIME_LEDGER_PERSIST_ERROR_SUMMARY;

    #[test]
    fn runtime_read_model_merges_sidecars_and_ledger_summary() {
        let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[SessionRuntimeSidecarExport {
                session_id: SessionId::new("session-1"),
                current_status: SessionExecutionSidecarStatus::Resumed,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership {
                    mission_id: Some(magi_core::MissionId::new("mission-1")),
                    task_id: Some(magi_core::TaskId::new("todo-1")),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-1".to_string()),
                recovery_ref: Some("recovery-1".to_string()),
                current_turn: None,
                active_execution_chain: None,
            }],
            &[WorkspaceRecoverySidecarExport {
                recovery_ref: "recovery-1".to_string(),
                workspace_id: WorkspaceId::new("workspace-1"),
                current_status: RecoveryStatus::Ready,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership {
                    session_id: Some(SessionId::new("session-1")),
                    mission_id: Some(magi_core::MissionId::new("mission-1")),
                    task_id: Some(magi_core::TaskId::new("todo-1")),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-1".to_string()),
                snapshot_id: "snapshot-1".to_string(),
                diagnostic_summary: Some("resume".to_string()),
                consumed_at: None,
            }],
            ledger_dto(AuditUsageLedgerStatus {
                schema_version: "audit-usage-ledger-v1".to_string(),
                next_sequence: 12,
                audit_count: 5,
                usage_count: 7,
                persistence_path: None,
                last_persist_error: None,
            }),
            None,
            &[],
        );

        assert_eq!(runtime_read_model.details.sessions.len(), 1);
        assert_eq!(
            runtime_read_model.details.sessions[0]
                .recovery_ref
                .as_deref(),
            Some("recovery-1")
        );
        assert_eq!(
            runtime_read_model.details.workspaces[0]
                .recovery_ref
                .as_deref(),
            Some("recovery-1")
        );
        assert_eq!(
            runtime_read_model.recovery.active_recovery_ids,
            vec!["recovery-1".to_string()]
        );
        assert_eq!(runtime_read_model.meta.ledger.next_sequence, 12);
        assert_eq!(runtime_read_model.meta.ledger.usage_count, 7);
    }

    #[test]
    fn runtime_read_model_sanitizes_turn_item_tool_text_only() {
        let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[SessionRuntimeSidecarExport {
                session_id: SessionId::new("session-tool-text"),
                current_status: SessionExecutionSidecarStatus::Bound,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership::default(),
                execution_chain_ref: None,
                recovery_ref: None,
                current_turn: Some(magi_session_store::ActiveExecutionTurn {
                    turn_id: "turn-tool-text".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(1),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("请检查 /Users/xie/code/TEST 里的文件".to_string()),
                    items: vec![magi_session_store::ActiveExecutionTurnItem {
                        item_id: "turn-item-tool".to_string(),
                        item_seq: 1,
                        kind: "tool_call".to_string(),
                        status: "failed".to_string(),
                        source: "worker".to_string(),
                        title: Some("读取文件".to_string()),
                        content: Some("用户要求检查 /Users/xie/code/TEST".to_string()),
                        task_id: None,
                        worker_id: None,
                        role_id: None,
                        tool_call_id: Some("tool-call-sensitive".to_string()),
                        tool_name: Some("read_file".to_string()),
                        tool_status: Some("failed".to_string()),
                        tool_arguments: Some(
                            r#"{"path":"/Users/xie/code/TEST/secret.txt","token":"sk-argument-secret"}"#
                                .to_string(),
                        ),
                        tool_result: Some(
                            "read /private/tmp/magi/result with Bearer resulttoken".to_string(),
                        ),
                        tool_error: Some(
                            "failed at /var/folders/magi/cache with sk-error-secret".to_string(),
                        ),
                        request_id: None,
                        user_message_id: None,
                        placeholder_message_id: None,
                        metadata: Default::default(),
                        timeline_entry_id: None,
                        source_thread_id: ThreadId::new("thread-tool-text"),
                    }],
                }),
                active_execution_chain: None,
            }],
            &[],
            ledger_dto(AuditUsageLedgerStatus::default()),
            None,
            &[],
        );

        let session = runtime_read_model
            .details
            .sessions
            .first()
            .expect("session summary should exist");
        let turn = session
            .current_turn
            .as_ref()
            .expect("current turn should exist");
        assert_eq!(
            turn.user_message.as_deref(),
            Some("请检查 /Users/xie/code/TEST 里的文件")
        );
        let item = session.turn_items.first().expect("turn item should exist");
        assert_eq!(
            item.content.as_deref(),
            Some("用户要求检查 /Users/xie/code/TEST")
        );

        let tool_arguments = item
            .tool_arguments
            .as_deref()
            .expect("tool arguments should exist");
        let tool_result = item
            .tool_result
            .as_deref()
            .expect("tool result should exist");
        let tool_error = item.tool_error.as_deref().expect("tool error should exist");
        for public_text in [tool_arguments, tool_result, tool_error] {
            assert!(public_text.contains("[path]"));
            assert!(!public_text.contains("/Users/xie"));
            assert!(!public_text.contains("/private/tmp"));
            assert!(!public_text.contains("/var/folders"));
            assert!(!public_text.contains("argument-secret"));
            assert!(!public_text.contains("resulttoken"));
            assert!(!public_text.contains("error-secret"));
        }
        assert!(tool_arguments.contains(r#""token":"[redacted]""#));
        assert!(tool_result.contains("Bearer [redacted]"));
        assert!(tool_error.contains("sk-[redacted]"));
    }

    #[test]
    fn runtime_read_model_uses_task_store_projection_for_active_execution_group() {
        let task_store = TaskStore::new();
        let mission_id = magi_core::MissionId::new("mission-projection-authority");
        let root_task_id = TaskId::new("task-root-projection");
        let running_task_id = TaskId::new("task-running-projection");
        let pending_task_id = TaskId::new("task-pending-projection");
        let completed_task_id = TaskId::new("task-completed-projection");
        let now = UtcMillis::now();
        for (task_id, parent_task_id, status) in [
            (&root_task_id, None, TaskStatus::Running),
            (
                &running_task_id,
                Some(root_task_id.clone()),
                TaskStatus::Running,
            ),
            (
                &pending_task_id,
                Some(root_task_id.clone()),
                TaskStatus::Pending,
            ),
            (
                &completed_task_id,
                Some(root_task_id.clone()),
                TaskStatus::Completed,
            ),
        ] {
            task_store.insert_task(magi_core::Task {
                task_id: task_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                parent_task_id,
                kind: if task_id == &root_task_id {
                    magi_core::TaskKind::LocalAgent
                } else {
                    magi_core::TaskKind::LocalAgent
                },
                title: task_id.to_string(),
                goal: task_id.to_string(),
                status,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                runtime_payload: magi_core::TaskRuntimePayload::default(),
                created_at: now,
                updated_at: now,
            });
        }

        let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[SessionRuntimeSidecarExport {
                session_id: SessionId::new("session-projection-authority"),
                current_status: SessionExecutionSidecarStatus::Bound,
                last_update: now,
                ownership: ExecutionOwnership {
                    session_id: Some(SessionId::new("session-projection-authority")),
                    mission_id: Some(mission_id.clone()),
                    task_id: Some(running_task_id.clone()),
                    execution_chain_ref: Some("chain-projection-authority".to_string()),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-projection-authority".to_string()),
                recovery_ref: None,
                current_turn: None,
                active_execution_chain: Some(magi_session_store::ActiveExecutionChain {
                    session_id: SessionId::new("session-projection-authority"),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-projection-authority".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![running_task_id.clone()],
                    active_worker_bindings: vec![magi_core::WorkerId::new("worker-projection")],
                    branches: vec![magi_session_store::ActiveExecutionBranch {
                        task_id: running_task_id.clone(),
                        worker_id: magi_core::WorkerId::new("worker-projection"),
                        stage: "execute".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: None,
                        checkpoint_stage: Some("execute".to_string()),
                        next_step_index: Some(0),
                        checkpoint_at: Some(now),
                        resume_mode: Some("stage-restart".to_string()),
                        resume_token: None,
                        use_tools: true,
                        skill_name: None,
                        is_primary: true,
                        thread_id: ThreadId::new("thread-projection-authority"),
                    }],
                    recovery_ref: None,
                    dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "entry-projection-authority".to_string(),
                        trimmed_text: Some("projection authority".to_string()),
                        skill_name: None,
                    },
                    current_turn: None,
                }),
            }],
            &[],
            ledger_dto(AuditUsageLedgerStatus::default()),
            Some(&task_store),
            &[],
        );

        let group = runtime_read_model
            .details
            .execution_groups
            .iter()
            .find(|entry| entry.mission_id == mission_id.to_string())
            .expect("TaskStore projection should create execution group entry");
        assert_eq!(group.current_status.as_deref(), Some("running"));
        assert!(group.active_task_ids.contains(&root_task_id.to_string()));
        assert!(group.active_task_ids.contains(&pending_task_id.to_string()));
        assert!(group.active_task_ids.contains(&running_task_id.to_string()));
        assert!(
            !group
                .active_task_ids
                .contains(&completed_task_id.to_string())
        );

        let task_statuses = runtime_read_model
            .details
            .tasks
            .iter()
            .map(|entry| (entry.task_id.as_str(), entry.current_status.as_deref()))
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            task_statuses
                .get(pending_task_id.as_str())
                .copied()
                .flatten(),
            Some("pending")
        );
        assert_eq!(
            task_statuses
                .get(completed_task_id.as_str())
                .copied()
                .flatten(),
            Some("completed")
        );
    }

    #[test]
    fn runtime_read_model_only_counts_failed_non_terminal_branches_as_recoverable() {
        let task_store = TaskStore::new();
        let mission_id = magi_core::MissionId::new("mission-recoverable-1");
        let root_task_id = magi_core::TaskId::new("task-root-recoverable-1");

        task_store.insert_task(magi_core::Task {
            task_id: root_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: magi_core::TaskKind::LocalAgent,
            title: "root".to_string(),
            goal: "root".to_string(),
            status: TaskStatus::Failed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        });
        for (task_id, status) in [
            ("task-branch-failed", TaskStatus::Failed),
            ("task-branch-pending", TaskStatus::Pending),
            ("task-branch-running", TaskStatus::Running),
            ("task-branch-completed", TaskStatus::Completed),
            ("task-branch-finished-failed", TaskStatus::Failed),
        ] {
            task_store.insert_task(magi_core::Task {
                task_id: magi_core::TaskId::new(task_id),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                parent_task_id: Some(root_task_id.clone()),
                kind: magi_core::TaskKind::LocalAgent,
                title: task_id.to_string(),
                goal: task_id.to_string(),
                status,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                runtime_payload: magi_core::TaskRuntimePayload::default(),
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            });
        }

        let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[SessionRuntimeSidecarExport {
                session_id: SessionId::new("session-recoverable-1"),
                current_status: SessionExecutionSidecarStatus::Bound,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership {
                    session_id: Some(SessionId::new("session-recoverable-1")),
                    mission_id: Some(mission_id.clone()),
                    task_id: Some(root_task_id.clone()),
                    execution_chain_ref: Some("chain-recoverable-1".to_string()),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-recoverable-1".to_string()),
                recovery_ref: None,
                current_turn: None,
                active_execution_chain: Some(magi_session_store::ActiveExecutionChain {
                    session_id: SessionId::new("session-recoverable-1"),
                    mission_id,
                    root_task_id,
                    execution_chain_ref: "chain-recoverable-1".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![
                        magi_core::TaskId::new("task-branch-failed"),
                        magi_core::TaskId::new("task-branch-pending"),
                        magi_core::TaskId::new("task-branch-running"),
                        magi_core::TaskId::new("task-branch-completed"),
                        magi_core::TaskId::new("task-branch-finished-failed"),
                    ],
                    active_worker_bindings: vec![magi_core::WorkerId::new("worker-recoverable-1")],
                    recovery_ref: None,
                    branches: vec![
                        magi_session_store::ActiveExecutionBranch {
                            task_id: magi_core::TaskId::new("task-branch-failed"),
                            worker_id: magi_core::WorkerId::new("worker-recoverable-1"),
                            stage: "execute".to_string(),
                            lease_id: None,
                            execution_intent_ref: None,
                            binding_lifecycle: None,
                            checkpoint_stage: Some("execute".to_string()),
                            next_step_index: Some(2),
                            checkpoint_at: Some(UtcMillis::now()),
                            resume_mode: Some("step-checkpoint".to_string()),
                            resume_token: None,
                            use_tools: true,
                            skill_name: None,
                            is_primary: true,
                            thread_id: ThreadId::new("thread-branch-failed"),
                        },
                        magi_session_store::ActiveExecutionBranch {
                            task_id: magi_core::TaskId::new("task-branch-pending"),
                            worker_id: magi_core::WorkerId::new("worker-recoverable-1"),
                            stage: "execute".to_string(),
                            lease_id: None,
                            execution_intent_ref: None,
                            binding_lifecycle: None,
                            checkpoint_stage: Some("execute".to_string()),
                            next_step_index: Some(0),
                            checkpoint_at: Some(UtcMillis::now()),
                            resume_mode: Some("stage-restart".to_string()),
                            resume_token: None,
                            use_tools: true,
                            skill_name: None,
                            is_primary: false,
                            thread_id: ThreadId::new("thread-branch-pending"),
                        },
                        magi_session_store::ActiveExecutionBranch {
                            task_id: magi_core::TaskId::new("task-branch-running"),
                            worker_id: magi_core::WorkerId::new("worker-recoverable-1"),
                            stage: "execute".to_string(),
                            lease_id: None,
                            execution_intent_ref: None,
                            binding_lifecycle: None,
                            checkpoint_stage: Some("execute".to_string()),
                            next_step_index: Some(1),
                            checkpoint_at: Some(UtcMillis::now()),
                            resume_mode: Some("step-checkpoint".to_string()),
                            resume_token: None,
                            use_tools: true,
                            skill_name: None,
                            is_primary: false,
                            thread_id: ThreadId::new("thread-branch-running"),
                        },
                        magi_session_store::ActiveExecutionBranch {
                            task_id: magi_core::TaskId::new("task-branch-completed"),
                            worker_id: magi_core::WorkerId::new("worker-recoverable-1"),
                            stage: "finish".to_string(),
                            lease_id: None,
                            execution_intent_ref: None,
                            binding_lifecycle: None,
                            checkpoint_stage: None,
                            next_step_index: None,
                            checkpoint_at: None,
                            resume_mode: None,
                            resume_token: None,
                            use_tools: true,
                            skill_name: None,
                            is_primary: false,
                            thread_id: ThreadId::new("thread-branch-completed"),
                        },
                        magi_session_store::ActiveExecutionBranch {
                            task_id: magi_core::TaskId::new("task-branch-finished-failed"),
                            worker_id: magi_core::WorkerId::new("worker-recoverable-1"),
                            stage: "finish".to_string(),
                            lease_id: None,
                            execution_intent_ref: None,
                            binding_lifecycle: None,
                            checkpoint_stage: None,
                            next_step_index: None,
                            checkpoint_at: None,
                            resume_mode: None,
                            resume_token: None,
                            use_tools: true,
                            skill_name: None,
                            is_primary: false,
                            thread_id: ThreadId::new("thread-branch-finished-failed"),
                        },
                    ],
                    dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                        accepted_at: UtcMillis::now(),
                        entry_id: "timeline-recoverable-1".to_string(),
                        trimmed_text: Some("recoverable".to_string()),
                        skill_name: None,
                    },
                    current_turn: None,
                }),
            }],
            &[],
            ledger_dto(AuditUsageLedgerStatus::default()),
            Some(&task_store),
            &[],
        );

        let session = &runtime_read_model.details.sessions[0];
        assert!(session.has_recoverable_chain);
        assert_eq!(session.recoverable_branch_count, 1);
        assert_eq!(session.active_branches.len(), 5);
    }

    #[test]
    fn runtime_read_model_excludes_completed_root_from_recoverable_chain() {
        let task_store = TaskStore::new();
        let mission_id = magi_core::MissionId::new("mission-terminal-recoverable");
        let root_task_id = magi_core::TaskId::new("task-root-terminal-recoverable");
        let failed_child_id = magi_core::TaskId::new("task-child-terminal-recoverable");
        let now = UtcMillis::now();

        for (task_id, parent_task_id, status) in [
            (root_task_id.clone(), None, TaskStatus::Completed),
            (
                failed_child_id.clone(),
                Some(root_task_id.clone()),
                TaskStatus::Failed,
            ),
        ] {
            task_store.insert_task(magi_core::Task {
                task_id: task_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                parent_task_id,
                kind: magi_core::TaskKind::LocalAgent,
                title: task_id.to_string(),
                goal: task_id.to_string(),
                status,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                runtime_payload: magi_core::TaskRuntimePayload::default(),
                created_at: now,
                updated_at: now,
            });
        }

        let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[SessionRuntimeSidecarExport {
                session_id: SessionId::new("session-terminal-recoverable"),
                current_status: SessionExecutionSidecarStatus::Bound,
                last_update: now,
                ownership: ExecutionOwnership {
                    session_id: Some(SessionId::new("session-terminal-recoverable")),
                    mission_id: Some(mission_id.clone()),
                    task_id: Some(root_task_id.clone()),
                    execution_chain_ref: Some("chain-terminal-recoverable".to_string()),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-terminal-recoverable".to_string()),
                recovery_ref: None,
                current_turn: None,
                active_execution_chain: Some(magi_session_store::ActiveExecutionChain {
                    session_id: SessionId::new("session-terminal-recoverable"),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-terminal-recoverable".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![root_task_id.clone(), failed_child_id.clone()],
                    active_worker_bindings: vec![magi_core::WorkerId::new(
                        "worker-terminal-recoverable",
                    )],
                    recovery_ref: None,
                    branches: vec![
                        magi_session_store::ActiveExecutionBranch {
                            task_id: root_task_id.clone(),
                            worker_id: magi_core::WorkerId::new("worker-terminal-recoverable"),
                            stage: "finish".to_string(),
                            lease_id: None,
                            execution_intent_ref: None,
                            binding_lifecycle: None,
                            checkpoint_stage: None,
                            next_step_index: None,
                            checkpoint_at: None,
                            resume_mode: None,
                            resume_token: None,
                            use_tools: true,
                            skill_name: None,
                            is_primary: true,
                            thread_id: ThreadId::new("thread-root-terminal-recoverable"),
                        },
                        magi_session_store::ActiveExecutionBranch {
                            task_id: failed_child_id.clone(),
                            worker_id: magi_core::WorkerId::new("worker-terminal-recoverable"),
                            stage: "execute".to_string(),
                            lease_id: None,
                            execution_intent_ref: None,
                            binding_lifecycle: None,
                            checkpoint_stage: Some("execute".to_string()),
                            next_step_index: Some(1),
                            checkpoint_at: Some(now),
                            resume_mode: Some("stage-restart".to_string()),
                            resume_token: None,
                            use_tools: true,
                            skill_name: None,
                            is_primary: false,
                            thread_id: ThreadId::new("thread-child-terminal-recoverable"),
                        },
                    ],
                    dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-terminal-recoverable".to_string(),
                        trimmed_text: Some("terminal recoverable".to_string()),
                        skill_name: None,
                    },
                    current_turn: None,
                }),
            }],
            &[],
            ledger_dto(AuditUsageLedgerStatus::default()),
            Some(&task_store),
            &[],
        );

        let session = runtime_read_model
            .details
            .sessions
            .iter()
            .find(|entry| entry.session_id == "session-terminal-recoverable")
            .expect("session summary should exist");
        assert!(!session.has_recoverable_chain);
        assert_eq!(session.recoverable_branch_count, 0);
        assert!(session.active_task_ids.is_empty());
        assert!(session.active_execution_group_ids.is_empty());
        assert!(
            runtime_read_model
                .overview
                .activity
                .active_task_ids
                .is_empty()
        );
        assert!(
            runtime_read_model
                .overview
                .activity
                .active_execution_group_ids
                .is_empty()
        );
    }

    #[test]
    fn runtime_read_model_keeps_runtime_ledger_signals_from_exported_summary() {
        let mut audit_usage_ledger = ledger_dto(AuditUsageLedgerStatus {
            schema_version: "audit-usage-ledger-v1".to_string(),
            next_sequence: 9,
            audit_count: 3,
            usage_count: 4,
            persistence_path: None,
            last_persist_error: Some("blocked".to_string()),
        });
        audit_usage_ledger.pending_flush = true;
        audit_usage_ledger.last_persisted_at = Some(UtcMillis::now());
        audit_usage_ledger.refresh_readiness();

        let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[],
            &[],
            audit_usage_ledger.clone(),
            None,
            &[],
        );

        assert_eq!(
            runtime_read_model.meta.ledger.schema_version,
            "audit-usage-ledger-v1"
        );
        assert_eq!(runtime_read_model.meta.ledger.audit_count, 3);
        assert_eq!(runtime_read_model.meta.ledger.usage_count, 4);
        assert_eq!(
            runtime_read_model.meta.ledger.last_persist_error.as_deref(),
            Some(RUNTIME_LEDGER_PERSIST_ERROR_SUMMARY)
        );
        assert!(runtime_read_model.meta.ledger.pending_flush);
        assert!(runtime_read_model.meta.ledger.last_persisted_at.is_some());
        assert_eq!(
            runtime_read_model.meta.ledger.cutover_readiness.is_ready,
            audit_usage_ledger.cutover_readiness.is_ready
        );
    }

    #[test]
    fn runtime_read_model_excludes_consumed_recoveries_from_active_ids() {
        let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[],
            &[
                WorkspaceRecoverySidecarExport {
                    recovery_ref: "recovery-ready".to_string(),
                    workspace_id: WorkspaceId::new("workspace-1"),
                    current_status: RecoveryStatus::Ready,
                    last_update: UtcMillis::now(),
                    ownership: ExecutionOwnership::default(),
                    execution_chain_ref: None,
                    snapshot_id: "snapshot-ready".to_string(),
                    diagnostic_summary: None,
                    consumed_at: None,
                },
                WorkspaceRecoverySidecarExport {
                    recovery_ref: "recovery-consumed".to_string(),
                    workspace_id: WorkspaceId::new("workspace-1"),
                    current_status: RecoveryStatus::Consumed,
                    last_update: UtcMillis::now(),
                    ownership: ExecutionOwnership::default(),
                    execution_chain_ref: None,
                    snapshot_id: "snapshot-consumed".to_string(),
                    diagnostic_summary: Some("done".to_string()),
                    consumed_at: Some(UtcMillis::now()),
                },
            ],
            ledger_dto(AuditUsageLedgerStatus::default()),
            None,
            &[],
        );

        assert_eq!(
            runtime_read_model.recovery.active_recovery_ids,
            vec!["recovery-ready".to_string()]
        );
        assert_eq!(runtime_read_model.recovery.summaries.len(), 2);
        assert_eq!(
            runtime_read_model.recovery.summaries[0].current_status,
            "consumed".to_string()
        );
        assert_eq!(
            runtime_read_model.recovery.summaries[1].current_status,
            "ready".to_string()
        );
    }

    #[test]
    fn runtime_read_model_sanitizes_workspace_recovery_diagnostic_summary() {
        let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[],
            &[WorkspaceRecoverySidecarExport {
                recovery_ref: "recovery-public-sidecar".to_string(),
                workspace_id: WorkspaceId::new("workspace-public-sidecar"),
                current_status: RecoveryStatus::Ready,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership::default(),
                execution_chain_ref: None,
                snapshot_id: "snapshot-public-sidecar".to_string(),
                diagnostic_summary: Some(
                    "resume from /Users/xie/.magi/recovery.json with Bearer abcdef and sk-test-secret"
                        .to_string(),
                ),
                consumed_at: None,
            }],
            ledger_dto(AuditUsageLedgerStatus::default()),
            None,
            &[],
        );

        let summary = runtime_read_model
            .recovery
            .summaries
            .first()
            .and_then(|summary| summary.diagnostic_summary.as_deref())
            .expect("recovery sidecar diagnostic should exist");
        assert!(summary.contains("[path]"));
        assert!(summary.contains("Bearer [redacted]"));
        assert!(summary.contains("sk-[redacted]"));
        assert!(!summary.contains("/Users/xie"));
        assert!(!summary.contains("abcdef"));
        assert!(!summary.contains("sk-test-secret"));
    }

    #[test]
    fn runtime_read_model_keeps_event_sourced_recovery_worker_when_workspace_sidecar_is_stale() {
        let mut input = RuntimeReadModelInput::default();
        input
            .recovery
            .summaries
            .push(RecoveryDiagnosticSummaryEntry {
                recovery_id: "recovery-worker-1".to_string(),
                event_count: 2,
                latest_stage: RecoveryActivityStage::WorkerResumed,
                latest_event_type: "worker.resumed.from_recovery".to_string(),
                latest_sequence: 3,
                latest_occurred_at: UtcMillis::now(),
                workspace_id: Some(WorkspaceId::new("workspace-1")),
                session_id: Some(magi_core::SessionId::new("session-1")),
                mission_id: Some(magi_core::MissionId::new("mission-1")),
                assignment_id: None,
                task_id: Some(magi_core::TaskId::new("todo-1")),
                worker_id: Some("worker-actual".to_string()),
                execution_chain_ref: Some("chain-1".to_string()),
                diagnostic_summary: Some("resume".to_string()),
                current_status: "worker_resumed".to_string(),
            });

        let runtime_read_model = runtime_read_model_dto(
            input,
            &[],
            &[WorkspaceRecoverySidecarExport {
                recovery_ref: "recovery-worker-1".to_string(),
                workspace_id: WorkspaceId::new("workspace-1"),
                current_status: RecoveryStatus::Consumed,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership {
                    worker_id: Some(magi_core::WorkerId::new("worker-stale")),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-1".to_string()),
                snapshot_id: "snapshot-1".to_string(),
                diagnostic_summary: Some("resume".to_string()),
                consumed_at: Some(UtcMillis::now()),
            }],
            ledger_dto(AuditUsageLedgerStatus::default()),
            None,
            &[],
        );

        assert_eq!(runtime_read_model.recovery.summaries.len(), 1);
        assert_eq!(
            runtime_read_model.recovery.summaries[0]
                .worker_id
                .as_deref(),
            Some("worker-actual")
        );
    }

    #[test]
    fn runtime_read_model_preserves_event_sourced_recovery_outcome_when_workspace_sidecar_is_consumed_snapshot()
     {
        let mut input = RuntimeReadModelInput::default();
        input
            .recovery
            .summaries
            .push(RecoveryDiagnosticSummaryEntry {
                recovery_id: "recovery-outcome-1".to_string(),
                event_count: 2,
                latest_stage: RecoveryActivityStage::WorkerResumed,
                latest_event_type: "worker.resumed.from_recovery".to_string(),
                latest_sequence: 7,
                latest_occurred_at: UtcMillis(50),
                workspace_id: None,
                session_id: Some(magi_core::SessionId::new("session-outcome-1")),
                mission_id: Some(magi_core::MissionId::new("mission-outcome-1")),
                assignment_id: None,
                task_id: Some(magi_core::TaskId::new("todo-outcome-1")),
                worker_id: Some("worker-outcome-1".to_string()),
                execution_chain_ref: Some("chain-outcome-1".to_string()),
                diagnostic_summary: None,
                current_status: "worker_resumed".to_string(),
            });

        let runtime_read_model = runtime_read_model_dto(
            input,
            &[],
            &[WorkspaceRecoverySidecarExport {
                recovery_ref: "recovery-outcome-1".to_string(),
                workspace_id: WorkspaceId::new("workspace-outcome-1"),
                current_status: RecoveryStatus::Consumed,
                last_update: UtcMillis(99),
                ownership: ExecutionOwnership {
                    session_id: Some(magi_core::SessionId::new("session-outcome-1")),
                    mission_id: Some(magi_core::MissionId::new("mission-stale")),
                    task_id: Some(magi_core::TaskId::new("todo-stale")),
                    worker_id: Some(magi_core::WorkerId::new("worker-stale")),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-stale".to_string()),
                snapshot_id: "snapshot-outcome-1".to_string(),
                diagnostic_summary: Some("resume detail".to_string()),
                consumed_at: Some(UtcMillis(99)),
            }],
            ledger_dto(AuditUsageLedgerStatus::default()),
            None,
            &[],
        );

        assert_eq!(runtime_read_model.recovery.summaries.len(), 1);
        let summary = &runtime_read_model.recovery.summaries[0];
        assert_eq!(summary.latest_stage, RecoveryActivityStage::WorkerResumed);
        assert_eq!(summary.current_status, "worker_resumed");
        assert_eq!(summary.latest_occurred_at, UtcMillis(50));
        assert_eq!(
            summary.workspace_id,
            Some(WorkspaceId::new("workspace-outcome-1"))
        );
        assert_eq!(summary.diagnostic_summary.as_deref(), Some("resume detail"));
        assert_eq!(summary.worker_id.as_deref(), Some("worker-outcome-1"));
        assert_eq!(
            summary.execution_chain_ref.as_deref(),
            Some("chain-outcome-1")
        );
    }

    #[test]
    fn bound_session_sidecar_does_not_mark_runtime_session_active() {
        let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[SessionRuntimeSidecarExport {
                session_id: SessionId::new("session-bound"),
                current_status: SessionExecutionSidecarStatus::Bound,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership {
                    mission_id: Some(magi_core::MissionId::new("mission-bound")),
                    task_id: Some(magi_core::TaskId::new("task-bound")),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-bound".to_string()),
                recovery_ref: None,
                current_turn: None,
                active_execution_chain: None,
            }],
            &[],
            ledger_dto(AuditUsageLedgerStatus::default()),
            None,
            &[],
        );

        assert_eq!(runtime_read_model.details.sessions.len(), 1);
        let session = &runtime_read_model.details.sessions[0];
        assert_eq!(session.session_id, "session-bound");
        assert_eq!(session.current_status.as_deref(), Some("bound"));
        assert!(session.active_execution_group_ids.is_empty());
        assert!(session.active_task_ids.is_empty());
        assert!(session.recovery_ids.is_empty());
        assert_eq!(session.execution_chain_ref.as_deref(), Some("chain-bound"));
    }

    #[test]
    fn session_sidecar_overrides_stale_event_scoped_active_ids() {
        let mission_id = magi_core::MissionId::new("mission-current");
        let root_task_id = magi_core::TaskId::new("task-root-current");
        let branch_task_id = magi_core::TaskId::new("task-branch-current");
        let worker_id = magi_core::WorkerId::new("worker-current");
        let root_created_at = UtcMillis::now();
        let branch_created_at = UtcMillis::now();
        let task_store = TaskStore::new();
        task_store.insert_task(magi_core::Task {
            task_id: root_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: magi_core::TaskKind::LocalAgent,
            title: "current root".to_string(),
            goal: "current root".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: root_created_at,
            updated_at: UtcMillis::now(),
        });
        task_store.insert_task(magi_core::Task {
            task_id: branch_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: Some(root_task_id.clone()),
            kind: magi_core::TaskKind::LocalAgent,
            title: "current branch".to_string(),
            goal: "current branch".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: branch_created_at,
            updated_at: UtcMillis::now(),
        });

        let mut input = RuntimeReadModelInput::default();
        input.details.sessions.push(SessionRuntimeSummaryEntry {
            session_id: "session-stale".to_string(),
            active_execution_group_ids: vec!["mission-stale".to_string()],
            active_task_ids: vec!["task-stale".to_string()],
            recovery_ids: vec!["recovery-stale".to_string()],
            current_status: Some("failed".to_string()),
            mission_id: Some("mission-stale".to_string()),
            root_task_id: Some("task-root-stale".to_string()),
            root_task_status: Some("failed".to_string()),
            execution_chain_ref: Some("chain-stale".to_string()),
            recovery_ref: Some("recovery-stale".to_string()),
            ..SessionRuntimeSummaryEntry::default()
        });

        let runtime_read_model = runtime_read_model_dto(
            input,
            &[SessionRuntimeSidecarExport {
                session_id: SessionId::new("session-stale"),
                current_status: SessionExecutionSidecarStatus::Bound,
                last_update: UtcMillis::now(),
                ownership: ExecutionOwnership {
                    session_id: Some(SessionId::new("session-stale")),
                    mission_id: Some(mission_id.clone()),
                    task_id: Some(branch_task_id.clone()),
                    worker_id: Some(worker_id.clone()),
                    execution_chain_ref: Some("chain-current".to_string()),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-current".to_string()),
                recovery_ref: None,
                current_turn: None,
                active_execution_chain: Some(magi_session_store::ActiveExecutionChain {
                    session_id: SessionId::new("session-stale"),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-current".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![branch_task_id.clone()],
                    active_worker_bindings: vec![worker_id.clone()],
                    recovery_ref: None,
                    branches: vec![magi_session_store::ActiveExecutionBranch {
                        task_id: branch_task_id.clone(),
                        worker_id: worker_id.clone(),
                        stage: "finish".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: None,
                        checkpoint_stage: None,
                        next_step_index: None,
                        checkpoint_at: None,
                        resume_mode: None,
                        resume_token: None,
                        use_tools: false,
                        skill_name: None,
                        is_primary: true,
                        thread_id: ThreadId::new("thread-chain-current"),
                    }],
                    dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                        accepted_at: UtcMillis::now(),
                        entry_id: "timeline-current".to_string(),
                        trimmed_text: Some("current".to_string()),
                        skill_name: None,
                    },
                    current_turn: None,
                }),
            }],
            &[],
            ledger_dto(AuditUsageLedgerStatus::default()),
            Some(&task_store),
            &[],
        );

        let session = runtime_read_model
            .details
            .sessions
            .iter()
            .find(|entry| entry.session_id == "session-stale")
            .expect("session summary should exist");
        assert!(session.active_execution_group_ids.is_empty());
        assert!(session.active_task_ids.is_empty());
        assert!(session.recovery_ids.is_empty());
        assert_eq!(session.current_status.as_deref(), Some("bound"));
        assert_eq!(session.mission_id.as_deref(), Some("mission-current"));
        assert_eq!(session.root_task_id.as_deref(), Some("task-root-current"));
        assert_eq!(session.root_task_status.as_deref(), Some("completed"));
        assert_eq!(session.root_task_created_at, Some(root_created_at));
        assert_eq!(
            session.execution_chain_ref.as_deref(),
            Some("chain-current")
        );
        assert!(session.current_turn.is_none());
        assert!(session.turn_items.is_empty());
    }

    #[test]
    fn session_sidecar_keeps_completed_current_turn_as_display_source() {
        let mission_id = magi_core::MissionId::new("mission-turn-completed");
        let root_task_id = magi_core::TaskId::new("task-root-turn-completed");
        let branch_task_id = magi_core::TaskId::new("task-branch-turn-completed");
        let worker_id = magi_core::WorkerId::new("worker-turn-completed");
        let accepted_at = UtcMillis::now();
        let task_store = TaskStore::new();
        task_store.insert_task(magi_core::Task {
            task_id: root_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: magi_core::TaskKind::LocalAgent,
            title: "completed root".to_string(),
            goal: "completed root".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: accepted_at,
            updated_at: accepted_at,
        });
        task_store.insert_task(magi_core::Task {
            task_id: branch_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: Some(root_task_id.clone()),
            kind: magi_core::TaskKind::LocalAgent,
            title: "completed branch".to_string(),
            goal: "completed branch".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: Some(serde_json::json!({
                "target_role": "reviewer",
                "capability_requirements": [],
                "parallelism_group": null,
                "exclusive_scope": null,
                "worker_selector": null,
            })),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: accepted_at,
            updated_at: accepted_at,
        });

        let runtime_read_model = runtime_read_model_dto(
            RuntimeReadModelInput::default(),
            &[SessionRuntimeSidecarExport {
                session_id: SessionId::new("session-turn-completed"),
                current_status: SessionExecutionSidecarStatus::Bound,
                last_update: accepted_at,
                ownership: ExecutionOwnership {
                    session_id: Some(SessionId::new("session-turn-completed")),
                    mission_id: Some(mission_id.clone()),
                    task_id: Some(branch_task_id.clone()),
                    worker_id: Some(worker_id.clone()),
                    execution_chain_ref: Some("chain-turn-completed".to_string()),
                    ..ExecutionOwnership::default()
                },
                execution_chain_ref: Some("chain-turn-completed".to_string()),
                recovery_ref: None,
                current_turn: Some(magi_session_store::ActiveExecutionTurn {
                    turn_id: "turn-session-action-completed".to_string(),
                    turn_seq: accepted_at.0,
                    accepted_at,
                    completed_at: None,
                    status: "completed".to_string(),
                    user_message: Some("请继续整理结果".to_string()),
                    items: vec![magi_session_store::ActiveExecutionTurnItem {
                        item_id: "turn-item-final".to_string(),
                        item_seq: 1,
                        kind: "assistant_final".to_string(),
                        status: "completed".to_string(),
                        source: "orchestrator".to_string(),
                        title: Some("总结".to_string()),
                        content: Some("这是 turn 主线最终总结".to_string()),
                        task_id: Some(branch_task_id.clone()),
                        worker_id: Some(worker_id.clone()),
                        role_id: None,
                        tool_call_id: None,
                        tool_name: None,
                        tool_status: None,
                        tool_arguments: None,
                        tool_result: None,
                        tool_error: None,
                        request_id: None,
                        user_message_id: None,
                        placeholder_message_id: None,
                        metadata: Default::default(),
                        timeline_entry_id: None,
                        source_thread_id: magi_core::ThreadId::new("thread-test-orchestrator"),
                    }],
                }),
                active_execution_chain: Some(magi_session_store::ActiveExecutionChain {
                    session_id: SessionId::new("session-turn-completed"),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-turn-completed".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![branch_task_id.clone()],
                    active_worker_bindings: vec![worker_id.clone()],
                    recovery_ref: None,
                    branches: vec![magi_session_store::ActiveExecutionBranch {
                        task_id: branch_task_id.clone(),
                        worker_id: worker_id.clone(),
                        stage: "finish".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: Some("completed".to_string()),
                        checkpoint_stage: None,
                        next_step_index: None,
                        checkpoint_at: None,
                        resume_mode: None,
                        resume_token: None,
                        use_tools: false,
                        skill_name: None,
                        is_primary: false,
                        thread_id: ThreadId::new("thread-turn-completed"),
                    }],
                    dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                        accepted_at,
                        entry_id: "timeline-turn-completed".to_string(),
                        trimmed_text: Some("已完成".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(magi_session_store::ActiveExecutionTurn {
                        turn_id: "turn-session-action-completed".to_string(),
                        turn_seq: accepted_at.0,
                        accepted_at,
                        completed_at: None,
                        status: "completed".to_string(),
                        user_message: Some("请继续整理结果".to_string()),
                        items: vec![magi_session_store::ActiveExecutionTurnItem {
                            item_id: "turn-item-final".to_string(),
                            item_seq: 1,
                            kind: "assistant_final".to_string(),
                            status: "completed".to_string(),
                            source: "orchestrator".to_string(),
                            title: Some("总结".to_string()),
                            content: Some("这是 turn 主线最终总结".to_string()),
                            task_id: Some(branch_task_id.clone()),
                            worker_id: Some(worker_id.clone()),
                            role_id: None,
                            tool_call_id: None,
                            tool_name: None,
                            tool_status: None,
                            tool_arguments: None,
                            tool_result: None,
                            tool_error: None,
                            request_id: None,
                            user_message_id: None,
                            placeholder_message_id: None,
                            metadata: Default::default(),
                            timeline_entry_id: None,
                            source_thread_id: magi_core::ThreadId::new("thread-test-orchestrator"),
                        }],
                    }),
                }),
            }],
            &[],
            ledger_dto(AuditUsageLedgerStatus::default()),
            Some(&task_store),
            &[],
        );

        let session = runtime_read_model
            .details
            .sessions
            .iter()
            .find(|entry| entry.session_id == "session-turn-completed")
            .expect("session summary should exist");
        assert!(session.active_execution_group_ids.is_empty());
        assert!(session.active_task_ids.is_empty());
        assert!(!session.active_branches.is_empty());
        assert!(!session.turn_items.is_empty());
        assert_eq!(
            session.turn_items[0].role_id.as_deref(),
            Some("reviewer"),
            "turn item role_id should be backfilled from task executor binding"
        );
        let current_turn = session
            .current_turn
            .as_ref()
            .expect("completed root should still preserve current turn display state");
        assert_eq!(current_turn.turn_id, "turn-session-action-completed");
        assert_eq!(current_turn.user_message.as_deref(), Some("请继续整理结果"));
        assert_eq!(session.root_task_status.as_deref(), Some("completed"));
        assert!(!session.has_recoverable_chain);
        assert_eq!(session.recoverable_branch_count, 0);
    }

    #[test]
    fn merge_mission_aggregates_patches_existing_entry_and_inserts_missing_one() {
        // 场景:dispatch projection 已经为 M-task 建好 entry(带 active task);
        // 而 M-charter 只有 charter,task projection 还没派发,merger 需要 find-or-insert。
        let mut input = RuntimeReadModelInput::default();
        input
            .details
            .execution_groups
            .push(ExecutionGroupRuntimeSummaryEntry {
                mission_id: "M-task".to_string(),
                event_count: 7,
                active_task_ids: vec!["task-1".to_string()],
                ..ExecutionGroupRuntimeSummaryEntry::default()
            });

        let exports = vec![
            MissionAggregateExport {
                mission_id: "M-task".to_string(),
                lifecycle_phase: MissionLifecyclePhase::Executing,
                metrics: Some(MissionMetrics {
                    schema_version: 1,
                    mission_id: magi_core::MissionId::new("M-task"),
                    turn_count: 3,
                    total_prompt_tokens: 120,
                    total_completion_tokens: 45,
                    total_tokens: 165,
                    first_turn_started_at: Some(UtcMillis(1_000)),
                    last_turn_finished_at: Some(UtcMillis(2_500)),
                    wall_clock_millis: 1_500,
                    last_lifecycle_phase: Some(MissionLifecyclePhase::Executing),
                }),
            },
            MissionAggregateExport {
                mission_id: "M-charter".to_string(),
                lifecycle_phase: MissionLifecyclePhase::PlanReady,
                metrics: None,
            },
        ];

        merge_mission_aggregates(&mut input, &exports);

        assert_eq!(input.details.execution_groups.len(), 2);
        let task_entry = input
            .details
            .execution_groups
            .iter()
            .find(|e| e.mission_id == "M-task")
            .expect("M-task entry must remain");
        assert_eq!(task_entry.event_count, 7, "已有字段必须保留");
        assert_eq!(task_entry.active_task_ids, vec!["task-1".to_string()]);
        assert_eq!(task_entry.lifecycle_phase.as_deref(), Some("executing"));
        let metrics = task_entry.metrics.as_ref().expect("metrics must be set");
        assert_eq!(metrics.turn_count, 3);
        assert_eq!(metrics.total_tokens, 165);
        assert_eq!(metrics.wall_clock_millis, 1_500);
        assert_eq!(metrics.last_lifecycle_phase.as_deref(), Some("executing"));

        let charter_entry = input
            .details
            .execution_groups
            .iter()
            .find(|e| e.mission_id == "M-charter")
            .expect("M-charter entry must be inserted");
        assert_eq!(charter_entry.lifecycle_phase.as_deref(), Some("plan_ready"));
        assert!(charter_entry.metrics.is_none());
        assert_eq!(
            charter_entry.event_count, 0,
            "新插入条目应使用默认零值,不污染计数"
        );
    }

    #[test]
    fn merge_mission_aggregates_overwrites_previous_lifecycle_phase() {
        // 守护:同一 mission 二次 export(例如轮询读 model)应覆盖上一次 lifecycle。
        let mut input = RuntimeReadModelInput::default();
        input
            .details
            .execution_groups
            .push(ExecutionGroupRuntimeSummaryEntry {
                mission_id: "M-evolving".to_string(),
                lifecycle_phase: Some("plan_ready".to_string()),
                ..ExecutionGroupRuntimeSummaryEntry::default()
            });

        merge_mission_aggregates(
            &mut input,
            &[MissionAggregateExport {
                mission_id: "M-evolving".to_string(),
                lifecycle_phase: MissionLifecyclePhase::AllStepsCompleted,
                metrics: None,
            }],
        );

        let entry = &input.details.execution_groups[0];
        assert_eq!(
            entry.lifecycle_phase.as_deref(),
            Some("all_steps_completed")
        );
    }
}
