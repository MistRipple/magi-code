use magi_core::{
    ApprovalRequirement, EventId, ExecutionResultStatus, RecoveryResumeInput,
    ResumeDispatchDecision, RiskLevel, TaskId, ToolCallId, UtcMillis, VerificationStatus,
    WorkerId, WorkerLifecycleStatus, WorkspaceId, SessionId,
};
use magi_governance::{GovernanceDecision, GovernanceOutcome, ToolKind, WorkerControlKind};
use magi_event_bus::{EventCategory, EventContext, EventEnvelope, InMemoryEventBus};
use magi_skill_runtime::{
    SkillDispatchRoute, SkillDispatchRuntime, SkillDispatchStatus, SkillToolRuntimePlan,
};
use magi_tool_runtime::ToolRegistry;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, RwLock},
};

mod executor;
mod executor_observation;
mod loop_controller;
mod local_process_executor;
mod reporting;
mod runtime_queries;
pub use executor::{
    DeterministicWorkerExecutor, ShadowWorkerExecutor, WorkerExecutionTrace,
    WorkerExecutorFailureDetail, WorkerExecutorKind, WorkerExecutorProbe,
};
pub use executor_observation::{
    WorkerExecutorObservation, WorkerExecutorObservationStatus,
};
pub use local_process_executor::{
    execute_intent_with_drivers, execute_intent_with_shadow_drivers, run_local_worker_executor_stdio,
    LocalProcessExecutionRequest, LocalProcessExecutionResponse, LocalProcessExecutorCapability,
    LocalProcessExecutorConfig, LocalProcessExecutorDescriptor, LocalProcessExecutorHealth,
    LocalProcessExecutorHealthStatus, LocalProcessExecutorAffinity,
    LocalProcessExecutorProcessModel, LocalProcessExecutorStageMatrix,
    LocalProcessProbeRequest, LocalProcessProbeResponse, LocalProcessProtocolRequest,
    LocalProcessProtocolRequestKind, LocalProcessProtocolResponse,
    LocalProcessProtocolResponseKind, LocalProcessRepairRequest, LocalProcessRepairResponse,
    LocalProcessReviewRequest, LocalProcessReviewResponse, LocalProcessVerifyRequest,
    LocalProcessVerifyResponse, LocalProcessWorkerExecutor, WorkerExecutionMode,
    WorkerExecutionBindingLifecycle, WorkerExecutionLeaseState, WorkerExecutionProcessLifecycle,
    WorkerExecutionBindingScope, WorkerExecutionParallelismScope, WorkerExecutionProfile,
    WorkerExecutionReusePolicy, WorkerExecutorFailure, WorkerExecutorFailureLayer,
};
pub use reporting::{
    SkillDispatchSummary, WorkerExecutionFinalReport, WorkerExecutionReport,
    WorkerSkillDispatchObservation, WorkerToolInvocation,
};
pub(crate) use reporting::derive_final_report;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerStage {
    Execute,
    Review,
    Verify,
    Repair,
    Finish,
}

impl WorkerStage {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Review => "review",
            Self::Verify => "verify",
            Self::Repair => "repair",
            Self::Finish => "finish",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerRecord {
    pub worker_id: WorkerId,
    pub current_task_id: Option<TaskId>,
    pub status: WorkerLifecycleStatus,
    pub stage: WorkerStage,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum WorkerExecutionStepKind {
    BuiltinToolInvocation,
    SkillDispatch,
    FinalReport,
}

impl WorkerExecutionStepKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::BuiltinToolInvocation => "builtin-tool-invocation",
            Self::SkillDispatch => "skill-dispatch",
            Self::FinalReport => "final-report",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerExecutionIntent {
    pub worker_id: WorkerId,
    pub task_id: TaskId,
    pub session_id: Option<SessionId>,
    pub workspace_id: Option<WorkspaceId>,
    pub execution_profile: WorkerExecutionProfile,
    pub steps: Vec<WorkerExecutionIntentStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerExecutorRequest {
    pub request_id: String,
    pub request_source: String,
    pub worker_id: WorkerId,
    pub task_id: TaskId,
    pub requested_stage: WorkerStage,
    pub session_id: Option<SessionId>,
    pub workspace_id: Option<WorkspaceId>,
    pub requested_execution_profile: WorkerExecutionProfile,
    pub requested_lease_state: WorkerExecutionLeaseState,
    pub requested_binding_lifecycle: WorkerExecutionBindingLifecycle,
    pub requested_process_lifecycle: WorkerExecutionProcessLifecycle,
    pub required_step_kinds: Vec<WorkerExecutionStepKind>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WorkerExecutionIntentStep {
    BuiltinToolInvocation {
        tool_call_id: ToolCallId,
        tool_name: String,
        tool_kind: ToolKind,
        input: String,
        approval_requirement: ApprovalRequirement,
        risk_level: RiskLevel,
        status: ExecutionResultStatus,
    },
    SkillDispatch {
        tool_call_id: ToolCallId,
        tool_name: String,
        plan: SkillToolRuntimePlan,
        payload: String,
        approval_requirement: ApprovalRequirement,
        risk_level: RiskLevel,
        working_directory: Option<String>,
        route: SkillDispatchRoute,
        binding_id: Option<String>,
        detail: String,
        status: SkillDispatchStatus,
    },
    FinalReport(WorkerExecutionFinalReport),
}

impl WorkerExecutionIntentStep {
    pub fn kind(&self) -> WorkerExecutionStepKind {
        match self {
            Self::BuiltinToolInvocation { .. } => WorkerExecutionStepKind::BuiltinToolInvocation,
            Self::SkillDispatch { .. } => WorkerExecutionStepKind::SkillDispatch,
            Self::FinalReport(_) => WorkerExecutionStepKind::FinalReport,
        }
    }
}

impl WorkerExecutionIntent {
    pub fn required_step_kinds(&self) -> Vec<WorkerExecutionStepKind> {
        let mut kinds = Vec::new();
        for step in &self.steps {
            let kind = step.kind();
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        kinds
    }

    pub fn executor_request(
        &self,
        requested_stage: WorkerStage,
        request_source: impl Into<String>,
    ) -> WorkerExecutorRequest {
        WorkerExecutorRequest {
            request_id: format!(
                "executor-request-{}-{}-{}",
                self.worker_id,
                self.task_id,
                UtcMillis::now().0
            ),
            request_source: request_source.into(),
            worker_id: self.worker_id.clone(),
            task_id: self.task_id.clone(),
            requested_stage,
            session_id: self.session_id.clone(),
            workspace_id: self.workspace_id.clone(),
            requested_execution_profile: self.execution_profile.clone(),
            requested_lease_state: self.execution_profile.lease_state,
            requested_binding_lifecycle: self.execution_profile.binding_lifecycle,
            requested_process_lifecycle: self.execution_profile.process_lifecycle,
            required_step_kinds: self.required_step_kinds(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkerGovernanceSummary {
    pub total_checks: usize,
    pub allowed: usize,
    pub needs_approval: usize,
    pub rejected: usize,
    pub blocked: usize,
    pub repair_retry: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerGovernanceObservation {
    pub worker_id: WorkerId,
    pub task_id: Option<TaskId>,
    pub action: WorkerControlKind,
    pub decision: GovernanceDecision,
    pub observed_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerRuntimeSummary {
    pub total_workers: usize,
    pub active_workers: usize,
    pub finished_workers: usize,
    pub failed_workers: usize,
    pub report_count: usize,
    pub tool_call_count: usize,
    pub skill_dispatch_count: usize,
    pub executor_observation_count: usize,
    pub latest_executor_status: Option<WorkerExecutorObservationStatus>,
    pub governance_count: usize,
    pub governance_summary: WorkerGovernanceSummary,
    pub skill_dispatch_summary: SkillDispatchSummary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerLoopAction {
    Execute {
        worker_id: WorkerId,
        task_id: TaskId,
    },
    Review {
        worker_id: WorkerId,
        summary: String,
    },
    Verify {
        worker_id: WorkerId,
        verification_status: VerificationStatus,
        summary: String,
    },
    Repair {
        worker_id: WorkerId,
        summary: String,
    },
    RepairRetry {
        worker_id: WorkerId,
        summary: String,
    },
    Finish {
        worker_id: WorkerId,
        summary: String,
    },
    Fail {
        worker_id: WorkerId,
        summary: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerLoopPlan {
    pub actions: Vec<WorkerLoopAction>,
}

impl WorkerLoopPlan {
    pub fn new(actions: Vec<WorkerLoopAction>) -> Self {
        Self { actions }
    }

    pub fn execution_cycle(
        worker_id: WorkerId,
        task_id: TaskId,
        review_summary: impl Into<String>,
        verification_status: VerificationStatus,
        repair_summary: impl Into<String>,
        finish_summary: impl Into<String>,
    ) -> Self {
        Self {
            actions: vec![
                WorkerLoopAction::Execute {
                    worker_id: worker_id.clone(),
                    task_id,
                },
                WorkerLoopAction::Review {
                    worker_id: worker_id.clone(),
                    summary: review_summary.into(),
                },
                WorkerLoopAction::Verify {
                    worker_id: worker_id.clone(),
                    verification_status,
                    summary: "verification checkpoint".to_string(),
                },
                WorkerLoopAction::Repair {
                    worker_id: worker_id.clone(),
                    summary: repair_summary.into(),
                },
                WorkerLoopAction::Finish {
                    worker_id,
                    summary: finish_summary.into(),
                },
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerLoopEntry {
    pub sequence: usize,
    pub action: WorkerLoopAction,
    pub governance_decision: Option<GovernanceDecision>,
    pub queued_at: UtcMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerLoopOutcomeKind {
    Applied,
    NeedsApproval,
    Blocked,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerLoopOutcome {
    pub sequence: usize,
    pub action: WorkerLoopAction,
    pub kind: WorkerLoopOutcomeKind,
    pub governance_decision: Option<GovernanceDecision>,
    pub worker: Option<WorkerRecord>,
    pub report: Option<WorkerExecutionReport>,
    pub rejection_reason: Option<String>,
    pub completed_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerExecutionSnapshot {
    pub worker: WorkerRecord,
    pub reports: Vec<WorkerExecutionReport>,
    pub tool_invocations: Vec<WorkerToolInvocation>,
    pub skill_dispatches: Vec<WorkerSkillDispatchObservation>,
    pub executor_observations: Vec<WorkerExecutorObservation>,
    pub governance_observations: Vec<WorkerGovernanceObservation>,
    pub governance_summary: WorkerGovernanceSummary,
    pub skill_dispatch_summary: SkillDispatchSummary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskExecutionSnapshot {
    pub task_id: TaskId,
    pub reports: Vec<WorkerExecutionReport>,
    pub tool_invocations: Vec<WorkerToolInvocation>,
    pub skill_dispatches: Vec<WorkerSkillDispatchObservation>,
    pub executor_observations: Vec<WorkerExecutorObservation>,
    pub governance_observations: Vec<WorkerGovernanceObservation>,
    pub governance_summary: WorkerGovernanceSummary,
    pub skill_dispatch_summary: SkillDispatchSummary,
}

#[derive(Clone)]
pub struct WorkerRuntime {
    event_bus: Arc<InMemoryEventBus>,
    workers: Arc<RwLock<HashMap<WorkerId, WorkerRecord>>>,
    reports: Arc<RwLock<Vec<WorkerExecutionReport>>>,
    tool_invocations: Arc<RwLock<Vec<WorkerToolInvocation>>>,
    skill_dispatches: Arc<RwLock<Vec<WorkerSkillDispatchObservation>>>,
    executor_observations: Arc<RwLock<Vec<WorkerExecutorObservation>>>,
    governance_observations: Arc<RwLock<Vec<WorkerGovernanceObservation>>>,
    execution_intents: Arc<RwLock<HashMap<TaskId, WorkerExecutionIntent>>>,
    executor: Arc<dyn ShadowWorkerExecutor>,
}

impl WorkerRuntime {
    pub fn new(event_bus: Arc<InMemoryEventBus>) -> Self {
        Self {
            event_bus,
            workers: Arc::new(RwLock::new(HashMap::new())),
            reports: Arc::new(RwLock::new(Vec::new())),
            tool_invocations: Arc::new(RwLock::new(Vec::new())),
            skill_dispatches: Arc::new(RwLock::new(Vec::new())),
            executor_observations: Arc::new(RwLock::new(Vec::new())),
            governance_observations: Arc::new(RwLock::new(Vec::new())),
            execution_intents: Arc::new(RwLock::new(HashMap::new())),
            executor: Arc::new(LocalProcessWorkerExecutor::cargo_loopback()),
        }
    }

    pub fn new_compare(event_bus: Arc<InMemoryEventBus>) -> Self {
        Self {
            event_bus,
            workers: Arc::new(RwLock::new(HashMap::new())),
            reports: Arc::new(RwLock::new(Vec::new())),
            tool_invocations: Arc::new(RwLock::new(Vec::new())),
            skill_dispatches: Arc::new(RwLock::new(Vec::new())),
            executor_observations: Arc::new(RwLock::new(Vec::new())),
            governance_observations: Arc::new(RwLock::new(Vec::new())),
            execution_intents: Arc::new(RwLock::new(HashMap::new())),
            executor: Arc::new(DeterministicWorkerExecutor::default()),
        }
    }

    pub fn with_executor(mut self, executor: Arc<dyn ShadowWorkerExecutor>) -> Self {
        self.executor = executor;
        self
    }

    pub fn register_worker(&self, worker_id: WorkerId) -> WorkerRecord {
        let record = WorkerRecord {
            worker_id: worker_id.clone(),
            current_task_id: None,
            status: WorkerLifecycleStatus::Idle,
            stage: WorkerStage::Execute,
            updated_at: UtcMillis::now(),
        };
        self.workers
            .write()
            .expect("worker runtime write lock poisoned")
            .insert(worker_id.clone(), record.clone());
        self.publish(
            "worker.registered",
            serde_json::json!({ "worker_id": worker_id.to_string() }),
        );
        record
    }

    pub fn start_execution(&self, worker_id: &WorkerId, task_id: TaskId) -> Option<WorkerRecord> {
        self.transition(worker_id, Some(task_id), WorkerLifecycleStatus::Running, WorkerStage::Execute)
    }

    pub fn resume_execution(
        &self,
        worker_id: &WorkerId,
        input: &RecoveryResumeInput,
    ) -> Option<WorkerRecord> {
        let task_id = input.ownership.task_id.clone()?;
        let record = self.transition(
            worker_id,
            Some(task_id.clone()),
            WorkerLifecycleStatus::Running,
            WorkerStage::Execute,
        )?;
        self.publish_with_category(
            "worker.resumed.from_recovery",
            EventCategory::Domain,
            EventContext {
                workspace_id: input.ownership.workspace_id.clone(),
                session_id: input.ownership.session_id.clone(),
                mission_id: input.ownership.mission_id.clone(),
                task_id: Some(task_id.clone()),
                ..EventContext::default()
            },
            serde_json::json!({
                "worker_id": worker_id.to_string(),
                "task_id": task_id.to_string(),
                "recovery_id": input.recovery_id,
                "execution_chain_ref": input.ownership.execution_chain_ref
            }),
        );
        Some(record)
    }

    pub fn resume_from_dispatch_decision(
        &self,
        decision: &ResumeDispatchDecision,
        fallback_worker_id: WorkerId,
    ) -> Option<WorkerRecord> {
        let worker_id = decision.worker_id.clone().unwrap_or(fallback_worker_id);
        self.ensure_worker_registered(&worker_id);
        let record = self.transition(
            &worker_id,
            Some(decision.task_id.clone()),
            WorkerLifecycleStatus::Running,
            WorkerStage::Execute,
        )?;
        self.publish_with_category(
            "worker.resumed.from_dispatch",
            EventCategory::Domain,
            EventContext {
                mission_id: Some(decision.mission_id.clone()),
                assignment_id: Some(decision.assignment_id.clone()),
                task_id: Some(decision.task_id.clone()),
                ..EventContext::default()
            },
            serde_json::json!({
                "worker_id": worker_id.to_string(),
                "task_id": decision.task_id.to_string(),
                "assignment_id": decision.assignment_id.to_string(),
                "mission_id": decision.mission_id.to_string(),
                "recovery_id": decision.recovery_id,
                "dispatch_reason": format!("{:?}", decision.dispatch_reason),
                "execution_chain_ref": decision.execution_chain_ref
            }),
        );
        Some(record)
    }

    pub fn start_review(&self, worker_id: &WorkerId) -> Option<WorkerRecord> {
        let task_id = self.current_task_id(worker_id)?;
        self.transition(worker_id, Some(task_id), WorkerLifecycleStatus::Reviewing, WorkerStage::Review)
    }

    pub fn start_verification(&self, worker_id: &WorkerId) -> Option<WorkerRecord> {
        let task_id = self.current_task_id(worker_id)?;
        self.transition(
            worker_id,
            Some(task_id),
            WorkerLifecycleStatus::Verifying,
            WorkerStage::Verify,
        )
    }

    pub fn start_repair(&self, worker_id: &WorkerId) -> Option<WorkerRecord> {
        let task_id = self.current_task_id(worker_id)?;
        self.transition(
            worker_id,
            Some(task_id),
            WorkerLifecycleStatus::Repairing,
            WorkerStage::Repair,
        )
    }

    pub fn register_execution_intent(&self, intent: WorkerExecutionIntent) {
        self.execution_intents
            .write()
            .expect("worker execution intent write lock poisoned")
            .insert(intent.task_id.clone(), intent);
    }

    pub fn execution_intent_for(&self, task_id: &TaskId) -> Option<WorkerExecutionIntent> {
        self.execution_intents
            .read()
            .expect("worker execution intent read lock poisoned")
            .get(task_id)
            .cloned()
    }

    pub fn executor_request_for(
        &self,
        worker_id: &WorkerId,
        task_id: &TaskId,
        requested_stage: WorkerStage,
        request_source: &str,
    ) -> Option<WorkerExecutorRequest> {
        self.execution_intent_for(task_id).map(|intent| {
            let mut request = intent.executor_request(requested_stage, request_source.to_string());
            request.worker_id = worker_id.clone();
            request
        })
    }

    pub fn workers(&self) -> Vec<WorkerRecord> {
        self.workers
            .read()
            .expect("worker runtime read lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    pub fn loop_controller(&self) -> WorkerRuntimeLoop {
        WorkerRuntimeLoop::new(self.clone())
    }

    pub(crate) fn transition(
        &self,
        worker_id: &WorkerId,
        task_id: Option<TaskId>,
        status: WorkerLifecycleStatus,
        stage: WorkerStage,
    ) -> Option<WorkerRecord> {
        let mut workers = self
            .workers
            .write()
            .expect("worker runtime write lock poisoned");
        let worker = workers.get_mut(worker_id)?;
        worker.current_task_id = task_id;
        worker.status = status;
        worker.stage = stage;
        worker.updated_at = UtcMillis::now();
        let snapshot = worker.clone();
        drop(workers);
        self.publish(
            "worker.transitioned",
            serde_json::json!({
                "worker_id": snapshot.worker_id.to_string(),
                "task_id": snapshot.current_task_id.as_ref().map(ToString::to_string),
                "status": format!("{:?}", snapshot.status),
                "stage": format!("{:?}", snapshot.stage)
            }),
        );
        Some(snapshot)
    }

    pub(crate) fn ensure_worker_registered(&self, worker_id: &WorkerId) {
        let exists = self
            .workers
            .read()
            .expect("worker runtime read lock poisoned")
            .contains_key(worker_id);
        if !exists {
            let _ = self.register_worker(worker_id.clone());
        }
    }

    pub(crate) fn current_task_id(&self, worker_id: &WorkerId) -> Option<TaskId> {
        self.workers
            .read()
            .expect("worker runtime read lock poisoned")
            .get(worker_id)
            .and_then(|worker| worker.current_task_id.clone())
    }

    fn publish(&self, event_type: &str, payload: serde_json::Value) {
        self.publish_with_category(
            event_type,
            EventCategory::System,
            EventContext::default(),
            payload,
        );
    }

    fn publish_with_category(
        &self,
        event_type: &str,
        category: EventCategory,
        context: EventContext,
        payload: serde_json::Value,
    ) {
        let base = match category {
            EventCategory::Domain => EventEnvelope::domain(
                EventId::new(format!("{event_type}-{}", UtcMillis::now().0)),
                event_type,
                payload,
            ),
            EventCategory::Audit => EventEnvelope::audit(
                EventId::new(format!("{event_type}-{}", UtcMillis::now().0)),
                event_type,
                payload,
            ),
            EventCategory::Usage => EventEnvelope::usage(
                EventId::new(format!("{event_type}-{}", UtcMillis::now().0)),
                event_type,
                payload,
            ),
            EventCategory::Projection => EventEnvelope::projection(
                EventId::new(format!("{event_type}-{}", UtcMillis::now().0)),
                event_type,
                payload,
            ),
            EventCategory::System => EventEnvelope::system(
                EventId::new(format!("{event_type}-{}", UtcMillis::now().0)),
                event_type,
                payload,
            ),
        };
        let _ = self.event_bus.publish(base.with_context(context));
    }
}

#[derive(Clone)]
pub struct WorkerRuntimeLoop {
    runtime: WorkerRuntime,
    tool_registry: Option<ToolRegistry>,
    skill_dispatch_runtime: Option<SkillDispatchRuntime>,
    queue: Arc<RwLock<VecDeque<WorkerLoopEntry>>>,
    history: Arc<RwLock<Vec<WorkerLoopOutcome>>>,
    next_sequence: Arc<RwLock<usize>>,
}

impl WorkerLoopAction {
    pub fn control_kind(&self) -> WorkerControlKind {
        match self {
            WorkerLoopAction::Execute { .. } => WorkerControlKind::Execute,
            WorkerLoopAction::Review { .. } => WorkerControlKind::Review,
            WorkerLoopAction::Verify { .. } => WorkerControlKind::Verify,
            WorkerLoopAction::Repair { .. } => WorkerControlKind::Repair,
            WorkerLoopAction::RepairRetry { .. } => WorkerControlKind::RepairRetry,
            WorkerLoopAction::Finish { .. } => WorkerControlKind::Finish,
            WorkerLoopAction::Fail { .. } => WorkerControlKind::Fail,
        }
    }

    pub fn worker_id(&self) -> &WorkerId {
        match self {
            WorkerLoopAction::Execute { worker_id, .. }
            | WorkerLoopAction::Review { worker_id, .. }
            | WorkerLoopAction::Verify { worker_id, .. }
            | WorkerLoopAction::Repair { worker_id, .. }
            | WorkerLoopAction::RepairRetry { worker_id, .. }
            | WorkerLoopAction::Finish { worker_id, .. }
            | WorkerLoopAction::Fail { worker_id, .. } => worker_id,
        }
    }

    pub fn current_task_id_hint(&self) -> Option<TaskId> {
        match self {
            WorkerLoopAction::Execute { task_id, .. } => Some(task_id.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{
        ApprovalRequirement, RiskLevel, TaskResultKind, TerminationReason,
        VerificationStatus, WorkerLifecycleStatus,
    };
    use magi_governance::{GovernanceService, WorkerControlRequest};
    use magi_event_bus::InMemoryEventBus;
    use std::sync::Arc;

    fn worker_id(value: &str) -> WorkerId {
        WorkerId::new(value.to_string())
    }

    fn task_id(value: &str) -> TaskId {
        TaskId::new(value.to_string())
    }

    #[test]
    fn worker_loop_can_run_success_cycle_step_by_step() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = WorkerRuntime::new_compare(bus);
        let loop_controller = runtime.loop_controller();
        let worker_id = worker_id("worker-1");
        let task_id = task_id("todo-1");

        let plan = WorkerLoopPlan::execution_cycle(
            worker_id.clone(),
            task_id.clone(),
            "reviewed",
            VerificationStatus::Passed,
            "repaired",
            "finished",
        );
        loop_controller.enqueue_plan(plan);

        let outcomes: Vec<_> = (0..5).filter_map(|_| loop_controller.step()).collect();
        assert_eq!(outcomes.len(), 5);
        assert!(loop_controller.is_idle());

        let summary = runtime.summary();
        assert_eq!(summary.total_workers, 1);
        assert_eq!(summary.active_workers, 0);
        assert_eq!(summary.finished_workers, 1);
        assert_eq!(summary.failed_workers, 0);
        assert_eq!(summary.report_count, 5);
        assert_eq!(summary.tool_call_count, 1);
        assert_eq!(summary.skill_dispatch_count, 1);
        assert_eq!(summary.executor_observation_count, 4);
        assert_eq!(
            summary.latest_executor_status,
            Some(WorkerExecutorObservationStatus::Ready)
        );

        let worker = runtime
            .workers()
            .into_iter()
            .find(|record| record.worker_id == worker_id)
            .expect("worker should exist");
        assert_eq!(worker.status, WorkerLifecycleStatus::Finished);
        assert_eq!(worker.current_task_id, Some(task_id));

        let reports = runtime.reports();
        assert_eq!(reports.len(), 5);
        let snapshot = runtime
            .snapshot_for_worker(&worker_id)
            .expect("worker snapshot should exist");
        assert_eq!(snapshot.executor_observations.len(), 4);
        assert_eq!(
            reports.last().expect("finish report missing").result_kind,
            Some(TaskResultKind::Success)
        );
    }

    #[test]
    fn worker_loop_uses_registered_execution_intent() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = WorkerRuntime::new_compare(bus);
        let loop_controller = runtime.loop_controller();
        let worker_id = worker_id("worker-1-intent");
        let task_id = task_id("todo-1-intent");

        runtime.register_worker(worker_id.clone());
        runtime.register_execution_intent(WorkerExecutionIntent {
            worker_id: worker_id.clone(),
            task_id: task_id.clone(),
            session_id: None,
            workspace_id: None,
            execution_profile: WorkerExecutionProfile::default(),
            steps: vec![
                WorkerExecutionIntentStep::BuiltinToolInvocation {
                    tool_call_id: ToolCallId::new("call-intent-1".to_string()),
                    tool_name: "file.read".to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: "{\"path\":\"README.md\"}".to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                    status: ExecutionResultStatus::Succeeded,
                },
                WorkerExecutionIntentStep::SkillDispatch {
                    tool_call_id: ToolCallId::new("call-intent-2".to_string()),
                    tool_name: "process.inspect".to_string(),
                    plan: DeterministicWorkerExecutor::default_skill_plan("process.inspect"),
                    payload: "{\"mode\":\"intent-skill\"}".to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                    working_directory: None,
                    route: SkillDispatchRoute::Builtin,
                    binding_id: None,
                    detail: "intent replay".to_string(),
                    status: SkillDispatchStatus::Succeeded,
                },
                WorkerExecutionIntentStep::FinalReport(WorkerExecutionFinalReport {
                    summary: "intent completed".to_string(),
                    result_kind: Some(TaskResultKind::Success),
                    termination_reason: Some(TerminationReason::Completed),
                    verification_status: VerificationStatus::Passed,
                }),
            ],
        });

        loop_controller.enqueue_action(WorkerLoopAction::Execute {
            worker_id: worker_id.clone(),
            task_id: task_id.clone(),
        });

        let outcome = loop_controller.step().expect("execute outcome should exist");
        assert_eq!(outcome.kind, WorkerLoopOutcomeKind::Applied);
        assert!(outcome.report.is_some());

        let summary = runtime.summary();
        assert_eq!(summary.tool_call_count, 1);
        assert_eq!(summary.skill_dispatch_count, 1);
        assert_eq!(summary.report_count, 1);

        let reports = runtime.reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(
            reports[0].summary,
            "intent completed".to_string()
        );
    }

    #[test]
    fn worker_loop_rejects_review_without_execution() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = WorkerRuntime::new_compare(bus);
        let loop_controller = runtime.loop_controller();
        let worker_id = worker_id("worker-2");

        loop_controller.enqueue_action(WorkerLoopAction::Review {
            worker_id: worker_id.clone(),
            summary: "reviewed".to_string(),
        });

        let outcome = loop_controller.step().expect("step should exist");
        assert_eq!(outcome.kind, WorkerLoopOutcomeKind::Rejected);
        assert!(outcome.rejection_reason.is_some());
        assert!(outcome.report.is_none());
        assert!(runtime.reports().is_empty());
    }

    #[test]
    fn worker_loop_rejects_review_when_executor_stage_is_disabled() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = WorkerRuntime::new_compare(bus).with_executor(Arc::new(
            LocalProcessWorkerExecutor::cargo_loopback()
                .with_env("MAGI_LOCAL_WORKER_STAGE_REVIEW", "false"),
        ));
        let loop_controller = runtime.loop_controller();
        let worker_id = worker_id("worker-review-stage-disabled");
        let task_id = task_id("todo-review-stage-disabled");

        runtime.register_worker(worker_id.clone());
        let _ = runtime.start_execution(&worker_id, task_id);

        loop_controller.enqueue_action(WorkerLoopAction::Review {
            worker_id: worker_id.clone(),
            summary: "reviewed".to_string(),
        });

        let outcome = loop_controller.step().expect("step should exist");
        assert_eq!(outcome.kind, WorkerLoopOutcomeKind::Rejected);
        assert!(outcome
            .rejection_reason
            .expect("rejection reason missing")
            .contains("executor capability insufficient"));
        assert!(outcome.report.is_none());
        assert!(runtime.reports().is_empty());
    }

    #[test]
    fn worker_loop_tracks_governance_block_and_repair_retry() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = WorkerRuntime::new_compare(bus);
        let loop_controller = runtime.loop_controller();
        let worker_id = worker_id("worker-3");
        let task_id = task_id("todo-3");

        let blocked_request = WorkerControlRequest {
            worker_id: Some(worker_id.clone()),
            mission_id: None,
            assignment_id: None,
            task_id: Some(task_id.clone()),
            action: WorkerControlKind::Execute,
            risk_level: RiskLevel::Low,
            approval_requirement: ApprovalRequirement::None,
            retry_count: 0,
            blocked: true,
            reason: Some("治理阻断".to_string()),
        };
        let blocked_decision = GovernanceService::default().evaluate_worker_control_request(&blocked_request);
        loop_controller.enqueue_guarded_action(
            WorkerLoopAction::Execute {
                worker_id: worker_id.clone(),
                task_id: task_id.clone(),
            },
            Some(blocked_decision),
        );

        let blocked_outcome = loop_controller.step().expect("blocked outcome should exist");
        assert_eq!(blocked_outcome.kind, WorkerLoopOutcomeKind::Blocked);
        assert!(blocked_outcome.worker.is_none());
        assert!(blocked_outcome.report.is_none());
        assert_eq!(runtime.governance_summary().blocked, 1);

        let _ = runtime.register_worker(worker_id.clone());
        let _ = runtime.start_execution(&worker_id, task_id.clone());

        let repair_retry_request = WorkerControlRequest {
            worker_id: Some(worker_id.clone()),
            mission_id: None,
            assignment_id: None,
            task_id: Some(task_id.clone()),
            action: WorkerControlKind::RepairRetry,
            risk_level: RiskLevel::Low,
            approval_requirement: ApprovalRequirement::None,
            retry_count: 1,
            blocked: false,
            reason: Some("repair retry".to_string()),
        };
        let repair_retry_decision =
            GovernanceService::default().evaluate_worker_control_request(&repair_retry_request);
        loop_controller.enqueue_guarded_action(
            WorkerLoopAction::RepairRetry {
                worker_id: worker_id.clone(),
                summary: "retry repair".to_string(),
            },
            Some(repair_retry_decision),
        );

        let retry_outcome = loop_controller.step().expect("repair retry outcome should exist");
        assert_eq!(retry_outcome.kind, WorkerLoopOutcomeKind::Applied);
        assert!(retry_outcome.report.is_some());
        assert_eq!(runtime.governance_summary().repair_retry, 1);
    }
}
