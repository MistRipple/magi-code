use crate::{
    EventCategory, EventContext, GovernanceDecision, GovernanceOutcome, SkillDispatchSummary,
    TaskExecutionSnapshot, TaskId, UtcMillis, WorkerControlKind, WorkerExecutionSnapshot,
    WorkerExecutorObservation, WorkerGovernanceObservation, WorkerGovernanceSummary, WorkerId,
    WorkerLifecycleStatus, WorkerRuntime, WorkerRuntimeBranchSnapshot,
    WorkerRuntimeDurableSnapshot, WorkerRuntimeSummary, WorkerSkillDispatchObservation,
    reporting::public_worker_skill_dispatch_observation,
};

impl WorkerRuntime {
    pub fn durable_snapshot(&self) -> WorkerRuntimeDurableSnapshot {
        let mut branches = self.branch_snapshots();
        branches.sort_by(|left, right| {
            left.task_id
                .as_str()
                .cmp(right.task_id.as_str())
                .then_with(|| left.worker_id.as_str().cmp(right.worker_id.as_str()))
        });
        WorkerRuntimeDurableSnapshot { branches }
    }

    pub fn branch_snapshots(&self) -> Vec<WorkerRuntimeBranchSnapshot> {
        self.branch_snapshots
            .read()
            .expect("worker branch snapshot read lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    pub fn branch_snapshot_for_task(
        &self,
        task_id: &TaskId,
    ) -> Option<WorkerRuntimeBranchSnapshot> {
        self.branch_snapshots
            .read()
            .expect("worker branch snapshot read lock poisoned")
            .get(task_id)
            .cloned()
    }

    pub fn skill_dispatches(&self) -> Vec<WorkerSkillDispatchObservation> {
        self.skill_dispatches
            .read()
            .expect("worker skill dispatch read lock poisoned")
            .iter()
            .cloned()
            .map(public_worker_skill_dispatch_observation)
            .collect()
    }

    pub fn skill_dispatch_summary(&self) -> SkillDispatchSummary {
        let skill_dispatches = self
            .skill_dispatches
            .read()
            .expect("worker skill dispatch read lock poisoned");
        SkillDispatchSummary::from_observations(skill_dispatches.iter())
    }

    pub fn skill_dispatch_summary_for_worker(&self, worker_id: &WorkerId) -> SkillDispatchSummary {
        let skill_dispatches = self
            .skill_dispatches
            .read()
            .expect("worker skill dispatch read lock poisoned");
        SkillDispatchSummary::from_observations(
            skill_dispatches
                .iter()
                .filter(|record| &record.worker_id == worker_id),
        )
    }

    pub fn skill_dispatch_summary_for_task(&self, task_id: &TaskId) -> SkillDispatchSummary {
        let skill_dispatches = self
            .skill_dispatches
            .read()
            .expect("worker skill dispatch read lock poisoned");
        SkillDispatchSummary::from_observations(
            skill_dispatches
                .iter()
                .filter(|record| &record.task_id == task_id),
        )
    }

    pub fn observe_governance_decision(
        &self,
        worker_id: &WorkerId,
        task_id: Option<TaskId>,
        action: WorkerControlKind,
        decision: GovernanceDecision,
    ) -> WorkerGovernanceObservation {
        let observation = public_worker_governance_observation(WorkerGovernanceObservation {
            worker_id: worker_id.clone(),
            task_id,
            action,
            decision,
            observed_at: UtcMillis::now(),
        });
        self.governance_observations
            .write()
            .expect("worker governance observation write lock poisoned")
            .push(observation.clone());
        self.publish_with_category(
            "worker.governance.observed",
            EventCategory::Audit,
            EventContext {
                task_id: observation.task_id.clone(),
                ..EventContext::default()
            },
            serde_json::json!({
                "worker_id": worker_id.to_string(),
                "task_id": observation.task_id.as_ref().map(ToString::to_string),
                "action": format!("{:?}", observation.action),
                "outcome": format!("{:?}", observation.decision.outcome),
                "phase": format!("{:?}", observation.decision.phase),
                "allowed": observation.decision.allowed,
                "requires_approval": observation.decision.requires_approval,
                "status": governance_outcome_status(&observation.decision.outcome),
                "reason": observation.decision.reason,
            }),
        );
        observation
    }

    pub fn governance_observations(&self) -> Vec<WorkerGovernanceObservation> {
        self.governance_observations
            .read()
            .expect("worker governance observation read lock poisoned")
            .iter()
            .cloned()
            .map(public_worker_governance_observation)
            .collect()
    }

    pub fn governance_summary(&self) -> WorkerGovernanceSummary {
        let observations = self
            .governance_observations
            .read()
            .expect("worker governance observation read lock poisoned");
        WorkerGovernanceSummary::from_observations(observations.iter())
    }

    pub fn governance_summary_for_worker(&self, worker_id: &WorkerId) -> WorkerGovernanceSummary {
        let observations = self
            .governance_observations
            .read()
            .expect("worker governance observation read lock poisoned");
        WorkerGovernanceSummary::from_observations(
            observations
                .iter()
                .filter(|observation| &observation.worker_id == worker_id),
        )
    }

    pub fn governance_summary_for_task(&self, task_id: &TaskId) -> WorkerGovernanceSummary {
        let observations = self
            .governance_observations
            .read()
            .expect("worker governance observation read lock poisoned");
        WorkerGovernanceSummary::from_observations(
            observations
                .iter()
                .filter(|observation| observation.task_id.as_ref() == Some(task_id)),
        )
    }

    pub fn snapshot_for_worker(&self, worker_id: &WorkerId) -> Option<WorkerExecutionSnapshot> {
        let worker = self
            .workers
            .read()
            .expect("worker runtime read lock poisoned")
            .get(worker_id)
            .cloned()?;
        let reports = self
            .reports
            .read()
            .expect("worker reports read lock poisoned")
            .iter()
            .filter(|report| &report.worker_id == worker_id)
            .cloned()
            .collect();
        let tool_invocations = self
            .tool_invocations
            .read()
            .expect("worker tool invocation read lock poisoned")
            .iter()
            .filter(|record| &record.worker_id == worker_id)
            .cloned()
            .collect();
        let skill_dispatches: Vec<WorkerSkillDispatchObservation> = self
            .skill_dispatches
            .read()
            .expect("worker skill dispatch read lock poisoned")
            .iter()
            .filter(|record| &record.worker_id == worker_id)
            .cloned()
            .map(public_worker_skill_dispatch_observation)
            .collect();
        let governance_observations: Vec<WorkerGovernanceObservation> = self
            .governance_observations
            .read()
            .expect("worker governance observation read lock poisoned")
            .iter()
            .filter(|record| &record.worker_id == worker_id)
            .cloned()
            .map(public_worker_governance_observation)
            .collect();
        let executor_observations: Vec<WorkerExecutorObservation> = self
            .executor_observations
            .read()
            .expect("worker executor observation read lock poisoned")
            .iter()
            .filter(|record| &record.worker_id == worker_id)
            .cloned()
            .collect();
        let skill_dispatch_summary =
            SkillDispatchSummary::from_observations(skill_dispatches.iter());
        let governance_summary =
            WorkerGovernanceSummary::from_observations(governance_observations.iter());
        Some(WorkerExecutionSnapshot {
            worker,
            reports,
            tool_invocations,
            skill_dispatches,
            executor_observations,
            governance_observations,
            governance_summary,
            skill_dispatch_summary,
        })
    }

    pub fn snapshot_for_task(&self, task_id: &TaskId) -> TaskExecutionSnapshot {
        let reports = self
            .reports
            .read()
            .expect("worker reports read lock poisoned")
            .iter()
            .filter(|report| &report.task_id == task_id)
            .cloned()
            .collect();
        let tool_invocations = self
            .tool_invocations
            .read()
            .expect("worker tool invocation read lock poisoned")
            .iter()
            .filter(|record| &record.task_id == task_id)
            .cloned()
            .collect();
        let skill_dispatches: Vec<WorkerSkillDispatchObservation> = self
            .skill_dispatches
            .read()
            .expect("worker skill dispatch read lock poisoned")
            .iter()
            .filter(|record| &record.task_id == task_id)
            .cloned()
            .map(public_worker_skill_dispatch_observation)
            .collect();
        let governance_observations: Vec<WorkerGovernanceObservation> = self
            .governance_observations
            .read()
            .expect("worker governance observation read lock poisoned")
            .iter()
            .filter(|record| record.task_id.as_ref() == Some(task_id))
            .cloned()
            .map(public_worker_governance_observation)
            .collect();
        let executor_observations: Vec<WorkerExecutorObservation> = self
            .executor_observations
            .read()
            .expect("worker executor observation read lock poisoned")
            .iter()
            .filter(|record| record.task_id.as_ref() == Some(task_id))
            .cloned()
            .collect();
        let skill_dispatch_summary =
            SkillDispatchSummary::from_observations(skill_dispatches.iter());
        let governance_summary =
            WorkerGovernanceSummary::from_observations(governance_observations.iter());
        TaskExecutionSnapshot {
            task_id: task_id.clone(),
            reports,
            tool_invocations,
            skill_dispatches,
            executor_observations,
            governance_observations,
            governance_summary,
            skill_dispatch_summary,
        }
    }

    pub fn summary(&self) -> WorkerRuntimeSummary {
        let workers = self
            .workers
            .read()
            .expect("worker runtime read lock poisoned");
        let total_workers = workers.len();
        let active_workers = workers
            .values()
            .filter(|worker| {
                matches!(
                    worker.status,
                    WorkerLifecycleStatus::Running
                        | WorkerLifecycleStatus::Reviewing
                        | WorkerLifecycleStatus::Verifying
                        | WorkerLifecycleStatus::Repairing
                )
            })
            .count();
        let finished_workers = workers
            .values()
            .filter(|worker| worker.status == WorkerLifecycleStatus::Finished)
            .count();
        let failed_workers = workers
            .values()
            .filter(|worker| worker.status == WorkerLifecycleStatus::Failed)
            .count();
        let report_count = self
            .reports
            .read()
            .expect("worker reports read lock poisoned")
            .len();
        let tool_call_count = self
            .tool_invocations
            .read()
            .expect("worker tool invocation read lock poisoned")
            .len();
        let skill_dispatch_count = self
            .skill_dispatches
            .read()
            .expect("worker skill dispatch read lock poisoned")
            .len();
        let executor_observation_count = self
            .executor_observations
            .read()
            .expect("worker executor observation read lock poisoned")
            .len();
        let latest_executor_status = self
            .executor_observations
            .read()
            .expect("worker executor observation read lock poisoned")
            .last()
            .map(|record| record.observation_status);
        let governance_count = self
            .governance_observations
            .read()
            .expect("worker governance observation read lock poisoned")
            .len();
        let governance_summary = self.governance_summary();
        let skill_dispatch_summary = self.skill_dispatch_summary();
        WorkerRuntimeSummary {
            total_workers,
            active_workers,
            finished_workers,
            failed_workers,
            report_count,
            tool_call_count,
            skill_dispatch_count,
            executor_observation_count,
            latest_executor_status,
            governance_count,
            governance_summary,
            skill_dispatch_summary,
        }
    }
}

fn governance_outcome_status(outcome: &GovernanceOutcome) -> &'static str {
    match outcome {
        GovernanceOutcome::Allowed => "allowed",
        GovernanceOutcome::NeedsApproval => "needs_approval",
        GovernanceOutcome::Rejected => "rejected",
        GovernanceOutcome::Blocked => "blocked",
    }
}

fn public_worker_governance_observation(
    mut observation: WorkerGovernanceObservation,
) -> WorkerGovernanceObservation {
    observation.decision.reason = Some(public_worker_governance_reason(
        &observation.action,
        &observation.decision,
    ));
    observation
}

fn public_worker_governance_reason(
    action: &WorkerControlKind,
    decision: &GovernanceDecision,
) -> String {
    match decision.outcome {
        GovernanceOutcome::Allowed => "worker 控制动作已通过治理检查".to_string(),
        GovernanceOutcome::NeedsApproval => "worker 控制动作需要人工审批".to_string(),
        GovernanceOutcome::Blocked => "worker 控制动作被治理阻断".to_string(),
        GovernanceOutcome::Rejected => match action {
            WorkerControlKind::RepairRetry => "修复重试不满足执行条件".to_string(),
            _ => "worker 控制动作被治理拒绝".to_string(),
        },
    }
}
