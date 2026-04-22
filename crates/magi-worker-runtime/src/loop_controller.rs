use crate::{
    execute_intent_with_drivers, SkillDispatchSummary, WorkerExecutionFinalReport,
    WorkerExecutionIntent, WorkerExecutionReport, WorkerExecutionTrace,
    WorkerExecutorFailure, WorkerGovernanceSummary, WorkerLoopAction, WorkerLoopEntry,
    WorkerLoopOutcome, WorkerLoopOutcomeKind, WorkerRecord, WorkerRuntime,
    WorkerRuntimeLoop, WorkerStage,
};
use magi_core::{TaskResultKind, TerminationReason, UtcMillis, VerificationStatus};
use magi_governance::{GovernanceDecision, GovernanceOutcome, WorkerControlKind};
use magi_skill_runtime::{SkillDispatchRoute, SkillDispatchRuntime, SkillDispatchStatus};
use magi_tool_runtime::ToolRegistry;
use std::{
    collections::VecDeque,
    sync::{Arc, RwLock},
};

impl WorkerRuntimeLoop {
    pub fn new(runtime: WorkerRuntime) -> Self {
        Self {
            runtime,
            tool_registry: None,
            skill_dispatch_runtime: None,
            queue: Arc::new(RwLock::new(VecDeque::new())),
            history: Arc::new(RwLock::new(Vec::new())),
            next_sequence: Arc::new(RwLock::new(0)),
        }
    }

    pub fn with_execution_drivers(
        mut self,
        tool_registry: ToolRegistry,
        skill_dispatch_runtime: SkillDispatchRuntime,
    ) -> Self {
        self.tool_registry = Some(tool_registry);
        self.skill_dispatch_runtime = Some(skill_dispatch_runtime);
        self
    }

    pub fn enqueue_action(&self, action: WorkerLoopAction) -> WorkerLoopEntry {
        self.enqueue_guarded_action(action, None)
    }

    pub fn enqueue_guarded_action(
        &self,
        action: WorkerLoopAction,
        governance_decision: Option<GovernanceDecision>,
    ) -> WorkerLoopEntry {
        let sequence = {
            let mut next_sequence = self
                .next_sequence
                .write()
                .expect("worker loop sequence write lock poisoned");
            let sequence = *next_sequence;
            *next_sequence += 1;
            sequence
        };
        let entry = WorkerLoopEntry {
            sequence,
            action,
            governance_decision,
            queued_at: UtcMillis::now(),
        };
        self.queue
            .write()
            .expect("worker loop queue write lock poisoned")
            .push_back(entry.clone());
        entry
    }

    pub fn enqueue_plan(&self, plan: crate::WorkerLoopPlan) -> Vec<WorkerLoopEntry> {
        plan.actions
            .into_iter()
            .map(|action| self.enqueue_action(action))
            .collect()
    }

    pub fn enqueue_guarded_plan(
        &self,
        plan: Vec<(WorkerLoopAction, GovernanceDecision)>,
    ) -> Vec<WorkerLoopEntry> {
        plan.into_iter()
            .map(|(action, decision)| self.enqueue_guarded_action(action, Some(decision)))
            .collect()
    }

    pub fn pending_actions(&self) -> Vec<WorkerLoopEntry> {
        self.queue
            .read()
            .expect("worker loop queue read lock poisoned")
            .iter()
            .cloned()
            .collect()
    }

    pub fn history(&self) -> Vec<WorkerLoopOutcome> {
        self.history
            .read()
            .expect("worker loop history read lock poisoned")
            .clone()
    }

    pub fn is_idle(&self) -> bool {
        self.queue
            .read()
            .expect("worker loop queue read lock poisoned")
            .is_empty()
    }

    pub fn step(&self) -> Option<WorkerLoopOutcome> {
        let entry = {
            let mut queue = self
                .queue
                .write()
                .expect("worker loop queue write lock poisoned");
            queue.pop_front()
        }?;
        let outcome = self.execute_entry(entry);
        self.history
            .write()
            .expect("worker loop history write lock poisoned")
            .push(outcome.clone());
        Some(outcome)
    }

    pub fn run_until_idle(&self) -> Vec<WorkerLoopOutcome> {
        let mut outcomes = Vec::new();
        while let Some(outcome) = self.step() {
            outcomes.push(outcome);
        }
        outcomes
    }

    fn execute_entry(&self, entry: WorkerLoopEntry) -> WorkerLoopOutcome {
        let WorkerLoopEntry {
            sequence,
            action,
            governance_decision,
            queued_at: _,
        } = entry;
        let completed_at = UtcMillis::now();
        let worker_id = action.worker_id().clone();
        let task_id_hint = action.current_task_id_hint();

        if let Some(decision) = governance_decision.clone() {
            let observation = self.runtime.observe_governance_decision(
                &worker_id,
                task_id_hint.clone().or_else(|| self.runtime.current_task_id(&worker_id)),
                action.control_kind(),
                decision.clone(),
            );
            match decision.outcome {
                GovernanceOutcome::Allowed => {}
                GovernanceOutcome::NeedsApproval => {
                    return self.governance_outcome(
                        sequence,
                        action,
                        Some(decision),
                        completed_at,
                        WorkerLoopOutcomeKind::NeedsApproval,
                        None,
                        None,
                        Some(format!(
                            "需要人工审批: {}",
                            observation
                                .decision
                                .reason
                                .clone()
                                .unwrap_or_else(|| "worker control action".to_string())
                        )),
                    );
                }
                GovernanceOutcome::Blocked => {
                    return self.governance_outcome(
                        sequence,
                        action,
                        Some(decision),
                        completed_at,
                        WorkerLoopOutcomeKind::Blocked,
                        None,
                        None,
                        Some(
                            observation
                                .decision
                                .reason
                                .clone()
                                .unwrap_or_else(|| "worker control 被治理阻断".to_string()),
                        ),
                    );
                }
                GovernanceOutcome::Rejected => {
                    return self.governance_outcome(
                        sequence,
                        action,
                        Some(decision),
                        completed_at,
                        WorkerLoopOutcomeKind::Rejected,
                        None,
                        None,
                        Some(
                            observation
                                .decision
                                .reason
                                .clone()
                                .unwrap_or_else(|| "worker control 被治理拒绝".to_string()),
                        ),
                    );
                }
            }
        }

        match action.clone() {
            WorkerLoopAction::Execute { worker_id, task_id } => {
                self.runtime.ensure_worker_registered(&worker_id);
                let intent = self
                    .runtime
                    .execution_intent_for(&task_id)
                    .unwrap_or_else(|| self.runtime.default_execution_intent(&worker_id, &task_id));
                let executor_request = intent.executor_request(WorkerStage::Execute, "execute");
                let executor_probe = self.runtime.executor_probe_for(Some(&executor_request));
                let _ = self.runtime.observe_executor_probe(
                    &worker_id,
                    Some(task_id.clone()),
                    Some(WorkerStage::Execute),
                    Some(&executor_request),
                    &executor_probe,
                );
                let executor_probe = match executor_probe {
                    Ok(probe) => probe,
                    Err(error) => {
                        return self.rejected_outcome(
                            sequence,
                            action,
                            governance_decision,
                            completed_at,
                            self.executor_probe_error_reason(&error),
                        );
                    }
                };
                if let Err(error) = executor_probe.supports_request(&executor_request) {
                    return self.rejected_outcome(
                        sequence,
                        action,
                        governance_decision,
                        completed_at,
                        &format!("executor capability insufficient: {error}"),
                    );
                }
                let worker = self.runtime.start_execution(&worker_id, task_id.clone());
                if worker.is_none() {
                    return self.rejected_outcome(
                        sequence,
                        action,
                        governance_decision,
                        completed_at,
                        "worker missing current task or not registered",
                    );
                }
                let trace = self.execute_intent_with_drivers(&intent).unwrap_or_else(|| {
                    self.runtime
                        .executor()
                        .execute_checked(&intent)
                        .unwrap_or_else(|error| WorkerExecutionTrace {
                            worker_id: intent.worker_id.clone(),
                            task_id: intent.task_id.clone(),
                            tool_invocations: Vec::new(),
                            skill_dispatches: Vec::new(),
                            final_report: WorkerExecutionFinalReport {
                                summary: format!(
                                    "external executor failed [{}]: {}",
                                    error.layer.label(),
                                    error.message
                                ),
                                result_kind: Some(TaskResultKind::Failure),
                                termination_reason: Some(TerminationReason::Failed),
                                verification_status: VerificationStatus::Failed,
                            },
                        })
                });

                for invocation in &trace.tool_invocations {
                    let _ = self.runtime.observe_tool_invocation(
                        &invocation.worker_id,
                        invocation.tool_call_id.clone(),
                        invocation.tool_name.clone(),
                        invocation.status,
                    );
                }
                for observation in &trace.skill_dispatches {
                    let _ = self.runtime.observe_skill_dispatch(&worker_id, observation.clone());
                }

                let final_status = match (
                    trace.final_report.result_kind,
                    trace.final_report.termination_reason,
                    trace.final_report.verification_status,
                ) {
                    (
                        Some(TaskResultKind::Success),
                        Some(TerminationReason::Completed),
                        VerificationStatus::Passed,
                    ) => crate::WorkerLifecycleStatus::Finished,
                    _ => crate::WorkerLifecycleStatus::Failed,
                };
                let final_worker = self.runtime.transition(
                    &worker_id,
                    Some(task_id.clone()),
                    final_status,
                    WorkerStage::Finish,
                );
                if final_worker.is_none() {
                    return self.rejected_outcome(
                        sequence,
                        action,
                        governance_decision,
                        completed_at,
                        "worker missing current task or not registered",
                    );
                }
                let report = self.runtime.append_report(
                    worker_id.clone(),
                    task_id,
                    WorkerStage::Finish,
                    trace.final_report.summary,
                    trace.final_report.result_kind,
                    trace.final_report.termination_reason,
                    trace.final_report.verification_status,
                );
                self.outcome(
                    sequence,
                    action,
                    governance_decision,
                    completed_at,
                    final_worker,
                    Some(report),
                    None,
                )
            }
            WorkerLoopAction::Review { worker_id, summary } => {
                let task_id = self.runtime.current_task_id(&worker_id);
                let executor_request = task_id.as_ref().and_then(|task_id| {
                    self.runtime
                        .executor_request_for(&worker_id, task_id, WorkerStage::Review, "review")
                });
                let executor_probe = self.runtime.executor_probe_for(executor_request.as_ref());
                let _ = self.runtime.observe_executor_probe(
                    &worker_id,
                    task_id.clone(),
                    Some(WorkerStage::Review),
                    executor_request.as_ref(),
                    &executor_probe,
                );
                let executor_probe = match executor_probe {
                    Ok(probe) => probe,
                    Err(error) => {
                        return self.rejected_outcome(
                            sequence,
                            action,
                            governance_decision,
                            completed_at,
                            self.executor_probe_error_reason(&error),
                        );
                    }
                };
                if let Some(request) = executor_request.as_ref() {
                    if let Err(error) = executor_probe.supports_request(request) {
                        return self.rejected_outcome(
                            sequence,
                            action,
                            governance_decision,
                            completed_at,
                            &format!("executor capability insufficient: {error}"),
                        );
                    }
                } else if let Err(error) = executor_probe.supports_stage(WorkerStage::Review) {
                    return self.rejected_outcome(
                        sequence,
                        action,
                        governance_decision,
                        completed_at,
                        &format!("executor capability insufficient: {error}"),
                    );
                }
                let worker = self.runtime.start_review(&worker_id);
                if worker.is_none() {
                    return self.rejected_outcome(
                        sequence,
                        action,
                        governance_decision,
                        completed_at,
                        "worker missing current task or not registered",
                    );
                }
                let report = self.runtime.record_review_note(&worker_id, summary);
                self.outcome(
                    sequence,
                    action,
                    governance_decision,
                    completed_at,
                    worker,
                    report,
                    None,
                )
            }
            WorkerLoopAction::Verify {
                worker_id,
                verification_status,
                summary,
            } => {
                let task_id = self.runtime.current_task_id(&worker_id);
                let executor_request = task_id.as_ref().and_then(|task_id| {
                    self.runtime
                        .executor_request_for(&worker_id, task_id, WorkerStage::Verify, "verify")
                });
                let executor_probe = self.runtime.executor_probe_for(executor_request.as_ref());
                let _ = self.runtime.observe_executor_probe(
                    &worker_id,
                    task_id.clone(),
                    Some(WorkerStage::Verify),
                    executor_request.as_ref(),
                    &executor_probe,
                );
                let executor_probe = match executor_probe {
                    Ok(probe) => probe,
                    Err(error) => {
                        return self.rejected_outcome(
                            sequence,
                            action,
                            governance_decision,
                            completed_at,
                            self.executor_probe_error_reason(&error),
                        );
                    }
                };
                if let Some(request) = executor_request.as_ref() {
                    if let Err(error) = executor_probe.supports_request(request) {
                        return self.rejected_outcome(
                            sequence,
                            action,
                            governance_decision,
                            completed_at,
                            &format!("executor capability insufficient: {error}"),
                        );
                    }
                } else if let Err(error) = executor_probe.supports_stage(WorkerStage::Verify) {
                    return self.rejected_outcome(
                        sequence,
                        action,
                        governance_decision,
                        completed_at,
                        &format!("executor capability insufficient: {error}"),
                    );
                }
                let worker = self.runtime.start_verification(&worker_id);
                if worker.is_none() {
                    return self.rejected_outcome(
                        sequence,
                        action,
                        governance_decision,
                        completed_at,
                        "worker missing current task or not registered",
                    );
                }
                let report = self
                    .runtime
                    .record_verification(&worker_id, verification_status, summary);
                self.outcome(
                    sequence,
                    action,
                    governance_decision,
                    completed_at,
                    worker,
                    report,
                    None,
                )
            }
            WorkerLoopAction::Repair { worker_id, summary } => {
                let task_id = self.runtime.current_task_id(&worker_id);
                let executor_request = task_id.as_ref().and_then(|task_id| {
                    self.runtime
                        .executor_request_for(&worker_id, task_id, WorkerStage::Repair, "repair")
                });
                let executor_probe = self.runtime.executor_probe_for(executor_request.as_ref());
                let _ = self.runtime.observe_executor_probe(
                    &worker_id,
                    task_id.clone(),
                    Some(WorkerStage::Repair),
                    executor_request.as_ref(),
                    &executor_probe,
                );
                let executor_probe = match executor_probe {
                    Ok(probe) => probe,
                    Err(error) => {
                        return self.rejected_outcome(
                            sequence,
                            action,
                            governance_decision,
                            completed_at,
                            self.executor_probe_error_reason(&error),
                        );
                    }
                };
                if let Some(request) = executor_request.as_ref() {
                    if let Err(error) = executor_probe.supports_request(request) {
                        return self.rejected_outcome(
                            sequence,
                            action,
                            governance_decision,
                            completed_at,
                            &format!("executor capability insufficient: {error}"),
                        );
                    }
                } else if let Err(error) = executor_probe.supports_stage(WorkerStage::Repair) {
                    return self.rejected_outcome(
                        sequence,
                        action,
                        governance_decision,
                        completed_at,
                        &format!("executor capability insufficient: {error}"),
                    );
                }
                let worker = self.runtime.start_repair(&worker_id);
                if worker.is_none() {
                    return self.rejected_outcome(
                        sequence,
                        action,
                        governance_decision,
                        completed_at,
                        "worker missing current task or not registered",
                    );
                }
                let report = self.runtime.record_repair_note(&worker_id, summary);
                self.outcome(
                    sequence,
                    action,
                    governance_decision,
                    completed_at,
                    worker,
                    report,
                    None,
                )
            }
            WorkerLoopAction::RepairRetry { worker_id, summary } => {
                let task_id = self.runtime.current_task_id(&worker_id);
                let executor_request = task_id.as_ref().and_then(|task_id| {
                    self.runtime.executor_request_for(
                        &worker_id,
                        task_id,
                        WorkerStage::Repair,
                        "repair-retry",
                    )
                });
                let executor_probe = self.runtime.executor_probe_for(executor_request.as_ref());
                let _ = self.runtime.observe_executor_probe(
                    &worker_id,
                    task_id.clone(),
                    Some(WorkerStage::Repair),
                    executor_request.as_ref(),
                    &executor_probe,
                );
                let executor_probe = match executor_probe {
                    Ok(probe) => probe,
                    Err(error) => {
                        return self.rejected_outcome(
                            sequence,
                            action,
                            governance_decision,
                            completed_at,
                            self.executor_probe_error_reason(&error),
                        );
                    }
                };
                if let Some(request) = executor_request.as_ref() {
                    if let Err(error) = executor_probe.supports_request(request) {
                        return self.rejected_outcome(
                            sequence,
                            action,
                            governance_decision,
                            completed_at,
                            &format!("executor capability insufficient: {error}"),
                        );
                    }
                } else if let Err(error) = executor_probe.supports_stage(WorkerStage::Repair) {
                    return self.rejected_outcome(
                        sequence,
                        action,
                        governance_decision,
                        completed_at,
                        &format!("executor capability insufficient: {error}"),
                    );
                }
                let worker = self.runtime.start_repair(&worker_id);
                if worker.is_none() {
                    return self.rejected_outcome(
                        sequence,
                        action,
                        governance_decision,
                        completed_at,
                        "worker missing current task or not registered",
                    );
                }
                let report = self
                    .runtime
                    .record_repair_note(&worker_id, format!("repair retry: {summary}"));
                self.outcome(
                    sequence,
                    action,
                    governance_decision,
                    completed_at,
                    worker,
                    report,
                    None,
                )
            }
            WorkerLoopAction::Finish { worker_id, summary } => {
                let worker = self.runtime.finish(&worker_id, summary);
                if worker.is_none() {
                    return self.rejected_outcome(
                        sequence,
                        action,
                        governance_decision,
                        completed_at,
                        "worker missing current task or not registered",
                    );
                }
                let task_id = worker
                    .as_ref()
                    .and_then(|record| record.current_task_id.clone())
                    .expect("finished worker should retain task id");
                let report = self
                    .runtime
                    .latest_report_for(&worker_id, &task_id, WorkerStage::Finish);
                self.outcome(
                    sequence,
                    action,
                    governance_decision,
                    completed_at,
                    worker,
                    report,
                    None,
                )
            }
            WorkerLoopAction::Fail { worker_id, summary } => {
                let worker = self.runtime.fail(&worker_id, summary);
                if worker.is_none() {
                    return self.rejected_outcome(
                        sequence,
                        action,
                        governance_decision,
                        completed_at,
                        "worker missing current task or not registered",
                    );
                }
                let task_id = worker
                    .as_ref()
                    .and_then(|record| record.current_task_id.clone())
                    .expect("failed worker should retain task id");
                let report = self
                    .runtime
                    .latest_report_for(&worker_id, &task_id, WorkerStage::Finish);
                self.outcome(
                    sequence,
                    action,
                    governance_decision,
                    completed_at,
                    worker,
                    report,
                    None,
                )
            }
        }
    }

    fn execute_intent_with_drivers(&self, intent: &WorkerExecutionIntent) -> Option<WorkerExecutionTrace> {
        let tool_registry = self.tool_registry.as_ref()?;
        let skill_dispatch_runtime = self.skill_dispatch_runtime.as_ref()?;
        Some(execute_intent_with_drivers(
            intent,
            tool_registry,
            skill_dispatch_runtime,
        ))
    }

    fn outcome(
        &self,
        sequence: usize,
        action: WorkerLoopAction,
        governance_decision: Option<GovernanceDecision>,
        completed_at: UtcMillis,
        worker: Option<WorkerRecord>,
        report: Option<WorkerExecutionReport>,
        rejection_reason: Option<String>,
    ) -> WorkerLoopOutcome {
        WorkerLoopOutcome {
            sequence,
            action,
            kind: if rejection_reason.is_some() {
                WorkerLoopOutcomeKind::Rejected
            } else {
                WorkerLoopOutcomeKind::Applied
            },
            governance_decision,
            worker,
            report,
            rejection_reason,
            completed_at,
        }
    }

    fn rejected_outcome(
        &self,
        sequence: usize,
        action: WorkerLoopAction,
        governance_decision: Option<GovernanceDecision>,
        completed_at: UtcMillis,
        rejection_reason: impl Into<String>,
    ) -> WorkerLoopOutcome {
        self.outcome(
            sequence,
            action,
            governance_decision,
            completed_at,
            None,
            None,
            Some(rejection_reason.into()),
        )
    }

    fn executor_probe_error_reason(&self, error: &WorkerExecutorFailure) -> String {
        match error.layer {
            crate::WorkerExecutorFailureLayer::RemoteBusiness => {
                format!("executor capability insufficient: {error}")
            }
            _ => format!("executor probe failed: {error}"),
        }
    }

    fn governance_outcome(
        &self,
        sequence: usize,
        action: WorkerLoopAction,
        governance_decision: Option<GovernanceDecision>,
        completed_at: UtcMillis,
        kind: WorkerLoopOutcomeKind,
        worker: Option<WorkerRecord>,
        report: Option<WorkerExecutionReport>,
        rejection_reason: Option<String>,
    ) -> WorkerLoopOutcome {
        WorkerLoopOutcome {
            sequence,
            action,
            kind,
            governance_decision,
            worker,
            report,
            rejection_reason,
            completed_at,
        }
    }
}

impl SkillDispatchSummary {
    pub fn from_observations<'a, I>(observations: I) -> Self
    where
        I: IntoIterator<Item = &'a crate::WorkerSkillDispatchObservation>,
    {
        let mut summary = SkillDispatchSummary::default();
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
}

impl WorkerGovernanceSummary {
    pub fn from_observations<'a, I>(observations: I) -> Self
    where
        I: IntoIterator<Item = &'a crate::WorkerGovernanceObservation>,
    {
        let mut summary = WorkerGovernanceSummary::default();
        for observation in observations {
            summary.total_checks += 1;
            if matches!(observation.action, WorkerControlKind::RepairRetry) {
                summary.repair_retry += 1;
            }
            match observation.decision.outcome {
                GovernanceOutcome::Allowed => summary.allowed += 1,
                GovernanceOutcome::NeedsApproval => summary.needs_approval += 1,
                GovernanceOutcome::Rejected => summary.rejected += 1,
                GovernanceOutcome::Blocked => summary.blocked += 1,
            }
        }
        summary
    }
}
