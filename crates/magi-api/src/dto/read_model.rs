use magi_core::{
    SessionId, TaskId, TaskStatus, public_runtime_excerpt, public_runtime_summary,
    public_runtime_text,
};
use magi_event_bus::{
    ExecutionGroupRuntimeSummaryEntry, RecoveryActivityStage, RecoveryDiagnosticSummaryEntry,
    RuntimeLedgerSummary, RuntimeReadModelInput, SessionRuntimeBranchSummaryEntry,
    SessionRuntimeBudgetEntry, SessionRuntimeSummaryEntry, SessionRuntimeTurnItemSummaryEntry,
    SessionRuntimeTurnSummaryEntry, SessionRuntimeUsageObservation, WorkspaceRuntimeSummaryEntry,
};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{SessionExecutionSidecarStatus, SessionRuntimeSidecarExport};

use magi_workspace::{RecoveryStatus, WorkspaceRecoverySidecarExport};
use std::collections::{BTreeMap, HashMap};

#[cfg(test)]
use magi_event_bus::AuditUsageLedgerStatus;

pub type RuntimeReadModelDto = RuntimeReadModelInput;
pub type AuditUsageLedgerDto = RuntimeLedgerSummary;

#[cfg(test)]
pub fn runtime_read_model_dto(
    runtime_read_model: RuntimeReadModelInput,
    session_sidecar_exports: &[SessionRuntimeSidecarExport],
    workspace_sidecar_exports: &[WorkspaceRecoverySidecarExport],
    audit_usage_ledger: AuditUsageLedgerDto,
    task_store: Option<&TaskStore>,
) -> RuntimeReadModelDto {
    runtime_read_model_dto_with_usage(
        runtime_read_model,
        session_sidecar_exports,
        workspace_sidecar_exports,
        audit_usage_ledger,
        task_store,
        &BTreeMap::new(),
    )
}

/// 从实时投影、持久会话和用量账本构造运行读模型。
///
/// 重启容错:守护进程重启后,event-bus 的 `recent_events` 缓冲区为空,实时投影
/// 无法为按 sidecar 重建的会话填充 `usage_observation`,预算因此整体丢失。生产
/// 装配路径用 `latest_usage_observations_from_ledger` 从已恢复账本算出
/// `ledger_usage_observations`,在 sidecar 合并之后、预算计算之前回填,使重启
/// 前后的 `budget` 口径一致。
#[allow(clippy::too_many_arguments)]
pub fn runtime_read_model_dto_with_usage(
    runtime_read_model: RuntimeReadModelInput,
    session_sidecar_exports: &[SessionRuntimeSidecarExport],
    workspace_sidecar_exports: &[WorkspaceRecoverySidecarExport],
    audit_usage_ledger: AuditUsageLedgerDto,
    task_store: Option<&TaskStore>,
    ledger_usage_observations: &BTreeMap<String, SessionRuntimeUsageObservation>,
) -> RuntimeReadModelDto {
    runtime_read_model_dto_inner(
        runtime_read_model,
        session_sidecar_exports,
        workspace_sidecar_exports,
        audit_usage_ledger,
        task_store,
        ledger_usage_observations,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn runtime_read_model_dto_inner(
    runtime_read_model: RuntimeReadModelInput,
    session_sidecar_exports: &[SessionRuntimeSidecarExport],
    workspace_sidecar_exports: &[WorkspaceRecoverySidecarExport],
    audit_usage_ledger: AuditUsageLedgerDto,
    task_store: Option<&TaskStore>,
    ledger_usage_observations: &BTreeMap<String, SessionRuntimeUsageObservation>,
    include_turn_detail: bool,
) -> RuntimeReadModelDto {
    let mut runtime_read_model = runtime_read_model;
    merge_session_sidecars(
        &mut runtime_read_model,
        session_sidecar_exports,
        task_store,
        include_turn_detail,
    );
    merge_workspace_sidecars(&mut runtime_read_model, workspace_sidecar_exports);
    backfill_session_usage_observations(&mut runtime_read_model, ledger_usage_observations);
    merge_session_budgets(&mut runtime_read_model);
    reconcile_runtime_task_activity(&mut runtime_read_model, task_store);
    runtime_read_model.meta.ledger = audit_usage_ledger;
    runtime_read_model
}

/// 构造单个会话的运行投影。
///
/// bootstrap 只展示当前会话，必须在 sidecar 脱敏和运行态合并之前完成范围裁剪。
/// 如果先构造全局读模型再裁剪，会对所有历史会话的工具参数、结果和错误重复执行
/// JSON 解析与脱敏，导致首屏耗时随全局历史线性增长。
#[allow(clippy::too_many_arguments)]
pub fn runtime_read_model_dto_for_session_with_usage(
    mut runtime_read_model: RuntimeReadModelInput,
    session_id: &SessionId,
    session_sidecar_exports: &[SessionRuntimeSidecarExport],
    workspace_sidecar_exports: &[WorkspaceRecoverySidecarExport],
    audit_usage_ledger: AuditUsageLedgerDto,
    task_store: Option<&TaskStore>,
    ledger_usage_observations: &BTreeMap<String, SessionRuntimeUsageObservation>,
) -> RuntimeReadModelDto {
    runtime_read_model
        .details
        .sessions
        .retain(|entry| entry.session_id == session_id.as_str());
    runtime_read_model
        .details
        .knowledge_audit
        .retain(|entry| entry.session_id.as_deref() == Some(session_id.as_str()));
    let scoped_sidecars = session_sidecar_exports
        .iter()
        .filter(|export| export.session_id == *session_id)
        .cloned()
        .collect::<Vec<_>>();

    // 当前回合的逐条明细（含每个工具的参数与结果）已由 canonicalTurns 承载，bootstrap
    // 不再重复构造一份运行态副本，否则单个大回合会让首屏成本翻倍。
    runtime_read_model_dto_inner(
        runtime_read_model,
        &scoped_sidecars,
        workspace_sidecar_exports,
        audit_usage_ledger,
        task_store,
        ledger_usage_observations,
        false,
    )
}

/// 仅当实时投影未提供观测值时,用账本回放结果回填会话的 `usage_observation`。
///
/// 反孤儿:实时压力快照优先(其负载与重启后账本同源同口径),
/// 账本只补齐重启后缺失的会话,避免覆盖更新的实时观测。
fn backfill_session_usage_observations(
    runtime_read_model: &mut RuntimeReadModelInput,
    ledger_usage_observations: &BTreeMap<String, SessionRuntimeUsageObservation>,
) {
    if ledger_usage_observations.is_empty() {
        return;
    }
    for session in &mut runtime_read_model.details.sessions {
        if session.usage_observation.is_some() {
            continue;
        }
        if let Some(observation) = ledger_usage_observations.get(&session.session_id) {
            session.usage_observation = Some(observation.clone());
        }
    }
}

#[cfg(test)]
pub fn ledger_dto(status: AuditUsageLedgerStatus) -> AuditUsageLedgerDto {
    RuntimeLedgerSummary::from(status)
}

/// 为每个会话投影上下文压力快照。
///
/// 窗口和 projected token 优先使用事件中绑定模型的快照，不能用当前活动模型
/// 覆盖历史调用。
fn merge_session_budgets(runtime_read_model: &mut RuntimeReadModelInput) {
    for session in &mut runtime_read_model.details.sessions {
        let Some(observation) = session.usage_observation.as_ref() else {
            session.budget = None;
            continue;
        };
        let (Some(context_window), Some(pressure_level)) = (
            observation
                .context_window_limit_tokens
                .filter(|window| *window > 0),
            observation.pressure_level.as_ref(),
        ) else {
            session.budget = None;
            continue;
        };
        let projected_tokens = observation.projected_request_tokens;
        let remaining_tokens = context_window.saturating_sub(projected_tokens);
        session.budget = Some(SessionRuntimeBudgetEntry {
            token_used: projected_tokens,
            remaining_tokens,
            token_limit: context_window,
            percent_remaining: (remaining_tokens.saturating_mul(100) / context_window.max(1))
                as i64,
            usage_ratio: projected_tokens as f64 / context_window.max(1) as f64,
            warning_level: pressure_level.clone(),
        });
    }
}

fn merge_session_sidecars(
    runtime_read_model: &mut RuntimeReadModelInput,
    session_sidecar_exports: &[SessionRuntimeSidecarExport],
    task_store: Option<&TaskStore>,
    include_turn_detail: bool,
) {
    // 事件窗口可能仍含旧绑定；只允许当前会话执行链重建活跃身份。
    for entry in &mut runtime_read_model.details.sessions {
        entry.active_task_ids.clear();
        entry.active_execution_group_ids.clear();
    }
    for export in session_sidecar_exports {
        let session_id = export.session_id.to_string();
        let tasks = export
            .active_execution_chain
            .as_ref()
            .and_then(|chain| {
                task_store.map(|store| store.execution_subtree_tasks(&chain.root_task_id))
            })
            .unwrap_or_default();
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
        entry.continuation = None;
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
            if let Some(task) = tasks.iter().find(|task| task.task_id == chain.root_task_id) {
                entry.root_task_status = Some(task_status_label(&task.status));
                entry.root_task_created_at = Some(task.created_at);
            }
            entry.active_branches = chain
                .branches
                .iter()
                .filter_map(|branch| {
                    tasks
                        .iter()
                        .find(|task| task.task_id == branch.task_id)
                        .map(|task| session_branch_summary(branch, task, task_store))
                })
                .collect();
            entry.recoverable_branch_count = chain.branches.iter().filter(|branch| {
                magi_conversation_runtime::execution_chain_recovery::active_execution_branch_is_continue_recoverable(
                    task_store, chain, export.current_turn.as_ref(), branch,
                )
            }).count();
            entry.has_recoverable_chain = entry.recoverable_branch_count > 0;
            if matches!(
                entry.root_task_status.as_deref(),
                Some("pending" | "running")
            ) {
                entry.active_task_ids = tasks
                    .iter()
                    .filter(|task| matches!(task.status, TaskStatus::Pending | TaskStatus::Running))
                    .map(|task| task.task_id.to_string())
                    .collect();
                entry
                    .active_execution_group_ids
                    .push(chain.mission_id.to_string());
            }
            push_unique(&mut entry.recovery_ids, export.recovery_ref.clone());
        }
        // 停止后能否继续：与继续动作共用同一份判定，客户端只消费这个结果。
        entry.continuation = crate::continuation::decide_continuation(
            entry.recoverable_branch_count,
            export.resume_candidate.as_ref(),
            task_store,
        )
        .map(|kind| kind.as_str().to_string());
        if include_turn_detail && let Some(turn) = export.current_turn.as_ref() {
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
            merge_task_store_projection(runtime_read_model, chain, &tasks);
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
    task: &magi_core::Task,
    task_store: Option<&TaskStore>,
) -> SessionRuntimeBranchSummaryEntry {
    let status = task_status_label(&task.status);
    let lease = task_store.and_then(|store| store.get_active_lease(&branch.task_id));
    SessionRuntimeBranchSummaryEntry {
        task_id: branch.task_id.to_string(),
        worker_id: branch.worker_id.to_string(),
        status,
        stage: match task.status {
            TaskStatus::Running => "execute",
            TaskStatus::Completed => "finish",
            TaskStatus::Failed => "failed",
            TaskStatus::Killed => "cancelled",
            TaskStatus::Pending => "pending",
        }
        .into(),
        lease_id: lease.map(|lease| lease.lease_id.to_string()),
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

/// 事件只提供诊断信息；活跃身份必须有当前 TaskStore 的 Pending/Running 事实。
fn reconcile_runtime_task_activity(runtime: &mut RuntimeReadModelInput, store: Option<&TaskStore>) {
    let tasks = store.map(TaskStore::all_tasks).unwrap_or_default();
    let by_id = tasks
        .iter()
        .map(|task| (task.task_id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let active = |id: &String| {
        by_id
            .get(id.as_str())
            .is_some_and(|task| matches!(task.status, TaskStatus::Pending | TaskStatus::Running))
    };
    let active_missions = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Pending | TaskStatus::Running))
        .map(|task| task.mission_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    for entry in &mut runtime.details.tasks {
        // 已删除或未恢复的 Task 不能由历史事件复活。
        entry.current_status = by_id
            .get(entry.task_id.as_str())
            .map(|task| task_status_label(&task.status));
    }
    runtime.overview.activity.active_task_ids = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Pending | TaskStatus::Running))
        .map(|task| task.task_id.to_string())
        .collect();
    runtime.overview.activity.active_task_ids.sort();
    runtime.overview.activity.active_execution_group_ids =
        active_missions.iter().map(|id| (*id).to_string()).collect();
    runtime.overview.activity.active_execution_group_ids.sort();
    for entry in &mut runtime.details.sessions {
        entry.active_task_ids.retain(active);
        entry
            .active_execution_group_ids
            .retain(|id| active_missions.contains(id.as_str()));
    }
    for entry in &mut runtime.details.workspaces {
        entry.active_task_ids.retain(active);
        entry
            .active_execution_group_ids
            .retain(|id| active_missions.contains(id.as_str()));
    }
    for entry in &mut runtime.details.execution_groups {
        entry.active_task_ids.retain(active);
    }
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
    chain: &magi_session_store::ActiveExecutionChain,
    tasks: &[magi_core::Task],
) {
    let Some(root) = tasks.iter().find(|task| task.task_id == chain.root_task_id) else {
        return;
    };
    let mission_id = root.mission_id.to_string();
    let active_task_ids = tasks
        .iter()
        .filter(|task| {
            !task_status_is_terminal(&root.status) && !task_status_is_terminal(&task.status)
        })
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
    group.current_status = Some(task_status_label(&root.status));
    group.active_task_ids.clear();
    for task_id in &active_task_ids {
        push_unique(&mut group.active_task_ids, Some(task_id.clone()));
    }

    if !task_status_is_terminal(&root.status) {
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

    for task in tasks {
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
        task_entry.failure_detail = if task.status == TaskStatus::Failed {
            task.output_refs
                .iter()
                .find(|value| !value.trim().is_empty())
                .map(|value| public_runtime_excerpt(value, 4096))
                .filter(|value| !value.is_empty())
        } else {
            None
        };
    }
}

fn task_status_is_terminal(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
    )
}

#[cfg(test)]
mod tests;
