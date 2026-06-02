use magi_core::{
    ApprovalRequirement, EventId, ExecutionResultStatus, RecoveryResumeInput, RiskLevel, SessionId,
    TaskExecutionTarget, TaskId, ToolCallId, UtcMillis, VerificationStatus, WorkerId,
    WorkerLifecycleStatus, WorkspaceId,
};
use magi_event_bus::{EventCategory, EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::{GovernanceDecision, GovernanceOutcome, ToolKind, WorkerControlKind};
use magi_skill_runtime::{
    SkillDispatchRoute, SkillDispatchRuntime, SkillDispatchStatus, SkillToolRuntimePlan,
};
use magi_tool_runtime::{ToolExecutionPolicy, ToolRegistry};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

mod executor;
mod executor_observation;
mod local_process_executor;
mod loop_controller;
mod reporting;
mod runtime_queries;
pub use executor::{
    DeterministicWorkerExecutor, WorkerExecutionProgress, WorkerExecutionTrace, WorkerExecutor,
    WorkerExecutorFailureDetail, WorkerExecutorKind, WorkerExecutorProbe,
};
pub use executor_observation::{WorkerExecutorObservation, WorkerExecutorObservationStatus};
pub use local_process_executor::{
    LocalProcessExecutionRequest, LocalProcessExecutionResponse, LocalProcessExecutorAffinity,
    LocalProcessExecutorCapability, LocalProcessExecutorConfig, LocalProcessExecutorDescriptor,
    LocalProcessExecutorHealth, LocalProcessExecutorHealthStatus, LocalProcessExecutorProcessModel,
    LocalProcessExecutorStageMatrix, LocalProcessProbeRequest, LocalProcessProbeResponse,
    LocalProcessProtocolRequest, LocalProcessProtocolRequestKind, LocalProcessProtocolResponse,
    LocalProcessProtocolResponseKind, LocalProcessRepairRequest, LocalProcessRepairResponse,
    LocalProcessReviewRequest, LocalProcessReviewResponse, LocalProcessVerifyRequest,
    LocalProcessVerifyResponse, LocalProcessWorkerExecutor, WorkerExecutionBindingLifecycle,
    WorkerExecutionBindingScope, WorkerExecutionLeaseState, WorkerExecutionMode,
    WorkerExecutionParallelismScope, WorkerExecutionProcessLifecycle, WorkerExecutionProfile,
    WorkerExecutionReusePolicy, WorkerExecutorFailure, WorkerExecutorFailureLayer,
    execute_intent_step_with_drivers, execute_intent_with_drivers,
    execute_intent_with_loopback_drivers, run_local_worker_executor_stdio,
};
pub(crate) use reporting::derive_final_report;
pub use reporting::{
    SkillDispatchSummary, WorkerExecutionFinalReport, WorkerExecutionReport,
    WorkerSkillDispatchObservation, WorkerToolInvocation,
};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerCheckpointResumeMode {
    StepCheckpoint,
    StageRestart,
}

impl WorkerCheckpointResumeMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::StepCheckpoint => "step-checkpoint",
            Self::StageRestart => "stage-restart",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerExecutionCheckpointCursor {
    pub checkpoint_stage: WorkerStage,
    pub next_step_index: usize,
    pub checkpoint_at: UtcMillis,
    pub resume_mode: WorkerCheckpointResumeMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_token: Option<String>,
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
    #[serde(default)]
    pub tool_policy: ToolExecutionPolicy,
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerRuntimeBranchSnapshot {
    pub task_id: TaskId,
    pub worker_id: WorkerId,
    pub stage: WorkerStage,
    pub lease_id: Option<String>,
    pub execution_intent_ref: Option<String>,
    pub binding_lifecycle: Option<WorkerExecutionBindingLifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_cursor: Option<WorkerExecutionCheckpointCursor>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerRuntimeDurableSnapshot {
    pub branches: Vec<WorkerRuntimeBranchSnapshot>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkerRuntimeSnapshotFlushState {
    pub current_version: u64,
    pub flushed_version: u64,
}

pub type WorkerBranchSnapshotObserver = Arc<dyn Fn(WorkerRuntimeBranchSnapshot) + Send + Sync>;

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
    branch_snapshots: Arc<RwLock<HashMap<TaskId, WorkerRuntimeBranchSnapshot>>>,
    branch_snapshot_observer: Arc<RwLock<Option<WorkerBranchSnapshotObserver>>>,
    durable_snapshot_version: Arc<AtomicU64>,
    flushed_durable_snapshot_version: Arc<AtomicU64>,
    executor: Arc<dyn WorkerExecutor>,
}

impl WorkerRuntime {
    fn default_checkpoint_cursor(
        stage: WorkerStage,
        checkpoint_at: UtcMillis,
    ) -> WorkerExecutionCheckpointCursor {
        WorkerExecutionCheckpointCursor {
            checkpoint_stage: stage,
            next_step_index: 0,
            checkpoint_at,
            resume_mode: WorkerCheckpointResumeMode::StageRestart,
            resume_token: None,
        }
    }

    pub fn checkpoint_cursor_for_task(
        &self,
        task_id: &TaskId,
    ) -> Option<WorkerExecutionCheckpointCursor> {
        self.branch_snapshot_for_task(task_id)
            .and_then(|snapshot| snapshot.checkpoint_cursor)
    }

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
            branch_snapshots: Arc::new(RwLock::new(HashMap::new())),
            branch_snapshot_observer: Arc::new(RwLock::new(None)),
            durable_snapshot_version: Arc::new(AtomicU64::new(0)),
            flushed_durable_snapshot_version: Arc::new(AtomicU64::new(0)),
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
            branch_snapshots: Arc::new(RwLock::new(HashMap::new())),
            branch_snapshot_observer: Arc::new(RwLock::new(None)),
            durable_snapshot_version: Arc::new(AtomicU64::new(0)),
            flushed_durable_snapshot_version: Arc::new(AtomicU64::new(0)),
            executor: Arc::new(DeterministicWorkerExecutor::default()),
        }
    }

    pub fn with_executor(mut self, executor: Arc<dyn WorkerExecutor>) -> Self {
        self.executor = executor;
        self
    }

    pub fn set_branch_snapshot_observer(&self, observer: Option<WorkerBranchSnapshotObserver>) {
        *self
            .branch_snapshot_observer
            .write()
            .expect("worker branch snapshot observer write lock poisoned") = observer;
    }

    pub fn record_branch_checkpoint(
        &self,
        task_id: &TaskId,
        worker_id: &WorkerId,
        stage: WorkerStage,
        lease_id: Option<String>,
        execution_intent_ref: Option<String>,
        binding_lifecycle: Option<WorkerExecutionBindingLifecycle>,
        checkpoint_cursor: Option<WorkerExecutionCheckpointCursor>,
    ) -> WorkerRuntimeBranchSnapshot {
        self.upsert_branch_snapshot(task_id, |existing| WorkerRuntimeBranchSnapshot {
            task_id: task_id.clone(),
            worker_id: worker_id.clone(),
            stage,
            lease_id: lease_id
                .clone()
                .or_else(|| existing.and_then(|snapshot| snapshot.lease_id.clone())),
            execution_intent_ref: execution_intent_ref
                .clone()
                .or_else(|| existing.and_then(|snapshot| snapshot.execution_intent_ref.clone())),
            binding_lifecycle: binding_lifecycle
                .or_else(|| existing.and_then(|snapshot| snapshot.binding_lifecycle)),
            checkpoint_cursor: if matches!(stage, WorkerStage::Finish) {
                None
            } else {
                checkpoint_cursor
                    .clone()
                    .or_else(|| existing.and_then(|snapshot| snapshot.checkpoint_cursor.clone()))
            },
        })
    }

    fn mark_durable_snapshot_dirty(&self) {
        self.durable_snapshot_version
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn durable_snapshot_flush_state(&self) -> WorkerRuntimeSnapshotFlushState {
        WorkerRuntimeSnapshotFlushState {
            current_version: self.durable_snapshot_version.load(Ordering::Relaxed),
            flushed_version: self
                .flushed_durable_snapshot_version
                .load(Ordering::Relaxed),
        }
    }

    pub fn durable_snapshot_dirty(&self) -> bool {
        let state = self.durable_snapshot_flush_state();
        state.current_version != state.flushed_version
    }

    fn publish_branch_snapshot(&self, snapshot: WorkerRuntimeBranchSnapshot) {
        let observer = self
            .branch_snapshot_observer
            .read()
            .expect("worker branch snapshot observer read lock poisoned")
            .clone();
        if let Some(observer) = observer {
            observer(snapshot);
        }
    }

    fn upsert_branch_snapshot(
        &self,
        task_id: &TaskId,
        mutate: impl FnOnce(Option<&WorkerRuntimeBranchSnapshot>) -> WorkerRuntimeBranchSnapshot,
    ) -> WorkerRuntimeBranchSnapshot {
        let snapshot = {
            let mut snapshots = self
                .branch_snapshots
                .write()
                .expect("worker branch snapshot write lock poisoned");
            let next = mutate(snapshots.get(task_id));
            snapshots.insert(task_id.clone(), next.clone());
            next
        };
        self.mark_durable_snapshot_dirty();
        self.publish_branch_snapshot(snapshot.clone());
        snapshot
    }

    fn restore_branch_snapshots(
        &self,
        branches: impl IntoIterator<Item = WorkerRuntimeBranchSnapshot>,
    ) {
        let mut branch_map = self
            .branch_snapshots
            .write()
            .expect("worker branch snapshot write lock poisoned");
        branch_map.clear();
        for branch in branches {
            branch_map.insert(branch.task_id.clone(), branch);
        }
        drop(branch_map);
        self.durable_snapshot_version.store(0, Ordering::Relaxed);
        self.flushed_durable_snapshot_version
            .store(0, Ordering::Relaxed);
    }

    pub fn restore_durable_snapshot(&self, snapshot: WorkerRuntimeDurableSnapshot) {
        self.restore_branch_snapshots(snapshot.branches);
    }

    pub fn flush_durable_snapshot_with<E, F>(&self, persist: F) -> Result<bool, E>
    where
        F: FnOnce(&WorkerRuntimeDurableSnapshot) -> Result<(), E>,
    {
        let current_version = self.durable_snapshot_version.load(Ordering::Relaxed);
        let flushed_version = self
            .flushed_durable_snapshot_version
            .load(Ordering::Relaxed);
        if current_version == flushed_version {
            return Ok(false);
        }
        let snapshot = self.durable_snapshot();
        persist(&snapshot)?;
        self.flushed_durable_snapshot_version
            .store(current_version, Ordering::Relaxed);
        Ok(true)
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
        self.transition(
            worker_id,
            Some(task_id),
            WorkerLifecycleStatus::Running,
            WorkerStage::Execute,
        )
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

    pub fn resume_from_execution_target(
        &self,
        target: &TaskExecutionTarget,
    ) -> Option<WorkerRecord> {
        let worker_id = target.requested_worker_id.clone()?;
        self.ensure_worker_registered(&worker_id);
        let record = self.transition(
            &worker_id,
            Some(target.task_id.clone()),
            WorkerLifecycleStatus::Running,
            WorkerStage::Execute,
        )?;
        self.publish_with_category(
            "worker.resumed.from_dispatch",
            EventCategory::Domain,
            EventContext {
                mission_id: Some(target.mission_id.clone()),
                task_id: Some(target.task_id.clone()),
                ..EventContext::default()
            },
            serde_json::json!({
                "worker_id": worker_id.to_string(),
                "task_id": target.task_id.to_string(),
                "mission_id": target.mission_id.to_string(),
                "recovery_id": target.recovery_id,
                "execution_chain_ref": target.execution_chain_ref
            }),
        );
        Some(record)
    }

    pub fn start_review(&self, worker_id: &WorkerId) -> Option<WorkerRecord> {
        let task_id = self.current_task_id(worker_id)?;
        self.transition(
            worker_id,
            Some(task_id),
            WorkerLifecycleStatus::Reviewing,
            WorkerStage::Review,
        )
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
        let task_id = intent.task_id.clone();
        let worker_id = intent.worker_id.clone();
        let binding_lifecycle = Some(intent.execution_profile.binding_lifecycle);
        let checkpoint_at = UtcMillis::now();
        self.execution_intents
            .write()
            .expect("worker execution intent write lock poisoned")
            .insert(intent.task_id.clone(), intent);
        self.record_branch_checkpoint(
            &task_id,
            &worker_id,
            self.branch_snapshot_for_task(&task_id)
                .map(|snapshot| snapshot.stage)
                .unwrap_or(WorkerStage::Execute),
            None,
            Some(format!("worker-intent-{task_id}")),
            binding_lifecycle,
            Some(Self::default_checkpoint_cursor(
                WorkerStage::Execute,
                checkpoint_at,
            )),
        );
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
        if let Some(task_id) = snapshot.current_task_id.clone() {
            let worker_id = snapshot.worker_id.clone();
            let checkpoint_cursor = match stage {
                WorkerStage::Execute
                | WorkerStage::Review
                | WorkerStage::Verify
                | WorkerStage::Repair => self
                    .branch_snapshot_for_task(&task_id)
                    .and_then(|branch| branch.checkpoint_cursor)
                    .filter(|cursor| cursor.checkpoint_stage == stage)
                    .or_else(|| Some(Self::default_checkpoint_cursor(stage, snapshot.updated_at))),
                WorkerStage::Finish => None,
            };
            self.record_branch_checkpoint(
                &task_id,
                &worker_id,
                stage,
                None,
                None,
                None,
                checkpoint_cursor,
            );
        }
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
        ApprovalRequirement, RiskLevel, TaskResultKind, TerminationReason, VerificationStatus,
        WorkerLifecycleStatus,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::{
        DecisionPhase, GovernanceDecision, GovernanceService, WorkerControlRequest,
    };
    use magi_tool_runtime::{BuiltinToolName, ToolExecutionContextQuery};
    use std::sync::Arc;

    fn worker_id(value: &str) -> WorkerId {
        WorkerId::new(value.to_string())
    }

    fn task_id(value: &str) -> TaskId {
        TaskId::new(value.to_string())
    }

    #[test]
    fn loopback_execution_uses_intent_tool_policy() {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let governance = Arc::new(GovernanceService::default());
        let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let skill_dispatch_runtime = SkillDispatchRuntime::new(
            tool_registry.clone(),
            magi_bridge_client::BridgeDispatchRuntime::new(),
        );
        let worker_id = worker_id("worker-loopback-policy");
        let task_id = task_id("task-loopback-policy");
        let intent = WorkerExecutionIntent {
            worker_id: worker_id.clone(),
            task_id: task_id.clone(),
            session_id: None,
            workspace_id: None,
            execution_profile: WorkerExecutionProfile::default(),
            tool_policy: ToolExecutionPolicy {
                access_profile: magi_core::AccessProfile::FullAccess,
                ..ToolExecutionPolicy::default()
            },
            steps: vec![WorkerExecutionIntentStep::BuiltinToolInvocation {
                tool_call_id: ToolCallId::new("tool-call-loopback-full-access-shell"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "printf loopback-full-access"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::Required,
                risk_level: RiskLevel::High,
                status: ExecutionResultStatus::Succeeded,
            }],
        };

        let trace = execute_intent_with_drivers(&intent, &tool_registry, &skill_dispatch_runtime);

        assert_eq!(trace.tool_invocations.len(), 1);
        assert_eq!(
            trace.tool_invocations[0].status,
            ExecutionResultStatus::Succeeded
        );
        let records = tool_registry.query_invocations(&ToolExecutionContextQuery {
            worker_id: Some(worker_id),
            task_id: Some(task_id),
            ..ToolExecutionContextQuery::default()
        });
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].context.access_profile,
            magi_core::AccessProfile::FullAccess
        );
    }

    #[derive(Clone)]
    struct FailingCheckpointExecutor;

    impl WorkerExecutor for FailingCheckpointExecutor {
        fn execute(&self, intent: &WorkerExecutionIntent) -> WorkerExecutionTrace {
            WorkerExecutionTrace {
                worker_id: intent.worker_id.clone(),
                task_id: intent.task_id.clone(),
                tool_invocations: Vec::new(),
                skill_dispatches: Vec::new(),
                final_report: WorkerExecutionFinalReport {
                    summary: "should not use raw executor failure".to_string(),
                    result_kind: Some(TaskResultKind::Failure),
                    termination_reason: Some(TerminationReason::Failed),
                    verification_status: VerificationStatus::Failed,
                },
            }
        }

        fn execute_from_checkpoint(
            &self,
            _intent: &WorkerExecutionIntent,
            _checkpoint_cursor: Option<&WorkerExecutionCheckpointCursor>,
        ) -> Result<WorkerExecutionProgress, WorkerExecutorFailure> {
            Err(WorkerExecutorFailure::remote_business(
                "/Users/xie/.magi/worker failed: ENOENT",
            ))
        }
    }

    #[test]
    fn worker_skill_dispatch_failure_detail_is_public_in_snapshot_and_event() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = WorkerRuntime::new_compare(bus.clone());
        let worker_id = worker_id("worker-skill-public-detail");
        let task_id = task_id("task-skill-public-detail");
        runtime.register_worker(worker_id.clone());
        runtime
            .start_execution(&worker_id, task_id.clone())
            .expect("worker should start execution");

        let raw_detail = "桥接调用失败[Transport]: /Users/xie/.mcp/server failed: ENOENT";
        let record = runtime
            .observe_skill_dispatch(
                &worker_id,
                magi_skill_runtime::SkillDispatchObservation {
                    tool_call_id: ToolCallId::new("skill-call-public-detail"),
                    tool_name: "echo.inspect".to_string(),
                    route: Some(SkillDispatchRoute::Bridge),
                    binding_id: Some("inspect-binding".to_string()),
                    bridge_kind: None,
                    dispatch_action: None,
                    status: SkillDispatchStatus::Failed,
                    error_kind: Some(magi_skill_runtime::SkillDispatchErrorKind::BridgeError),
                    bridge_error_layer: Some(magi_bridge_client::BridgeErrorLayer::Transport),
                    bridge_error_message: Some(raw_detail.to_string()),
                    detail: raw_detail.to_string(),
                },
            )
            .expect("skill dispatch should be recorded");

        assert_eq!(
            record.detail,
            "Skill 工具调用失败，请检查工具配置或外接服务状态"
        );
        assert!(!record.detail.contains("/Users/xie"));
        assert!(!record.detail.contains("ENOENT"));

        let event = bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "worker.skill_dispatch.observed")
            .expect("skill dispatch event should be published");
        let event_text = event.payload.to_string();
        assert!(event_text.contains("Skill 工具调用失败"));
        assert!(
            !event_text.contains("/Users/xie") && !event_text.contains("ENOENT"),
            "worker skill dispatch event must not expose raw runtime detail: {event_text}"
        );

        let snapshot = runtime
            .snapshot_for_task(&task_id)
            .skill_dispatches
            .into_iter()
            .next()
            .expect("skill dispatch should be present in task snapshot");
        assert_eq!(snapshot.detail, record.detail);
    }

    #[test]
    fn worker_executor_failure_observation_is_public_in_snapshot_and_event() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = WorkerRuntime::new_compare(bus.clone());
        let worker_id = worker_id("worker-executor-public-failure");
        let task_id = task_id("task-executor-public-failure");
        let raw_detail = "/Users/xie/.magi/worker failed: ENOENT";
        let probe_result = Err(WorkerExecutorFailure::remote_business(raw_detail));

        let record = runtime.observe_executor_probe(
            &worker_id,
            Some(task_id.clone()),
            Some(WorkerStage::Execute),
            None,
            &probe_result,
        );

        assert_eq!(
            record.failure_message.as_deref(),
            Some("executor capability insufficient")
        );
        assert!(
            !record
                .failure_message
                .as_deref()
                .unwrap_or_default()
                .contains("/Users/xie")
        );

        let event = bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "worker.executor.observed")
            .expect("executor observation event should be published");
        let event_text = event.payload.to_string();
        assert!(event_text.contains("executor capability insufficient"));
        assert!(
            !event_text.contains("/Users/xie") && !event_text.contains("ENOENT"),
            "worker executor event must not expose raw runtime detail: {event_text}"
        );

        let snapshot_text = serde_json::to_string(&runtime.snapshot_for_task(&task_id))
            .expect("task snapshot should serialize");
        assert!(
            !snapshot_text.contains("/Users/xie") && !snapshot_text.contains("ENOENT"),
            "worker executor snapshot must not expose raw runtime detail: {snapshot_text}"
        );
    }

    #[test]
    fn worker_loop_executor_step_failure_report_uses_public_summary() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = WorkerRuntime::new_compare(bus.clone())
            .with_executor(Arc::new(FailingCheckpointExecutor));
        let loop_controller = runtime.loop_controller();
        let worker_id = worker_id("worker-executor-public-report");
        let task_id = task_id("task-executor-public-report");

        runtime.register_worker(worker_id.clone());
        runtime.register_execution_intent(WorkerExecutionIntent {
            worker_id: worker_id.clone(),
            task_id: task_id.clone(),
            session_id: None,
            workspace_id: None,
            execution_profile: WorkerExecutionProfile::default(),
            tool_policy: ToolExecutionPolicy::default(),
            steps: vec![WorkerExecutionIntentStep::FinalReport(
                WorkerExecutionFinalReport {
                    summary: "should be replaced by executor failure".to_string(),
                    result_kind: Some(TaskResultKind::Success),
                    termination_reason: Some(TerminationReason::Completed),
                    verification_status: VerificationStatus::Passed,
                },
            )],
        });

        loop_controller.enqueue_action(WorkerLoopAction::Execute {
            worker_id,
            task_id: task_id.clone(),
        });

        let outcome = loop_controller
            .step()
            .expect("execute outcome should exist");
        let report = outcome.report.expect("failure report should be recorded");
        assert_eq!(
            report.summary,
            "external executor failed because the executor capability is insufficient"
        );
        assert!(!report.summary.contains("/Users/xie"));

        let event_text = bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "worker.reported")
            .expect("worker report event should be published")
            .payload
            .to_string();
        assert!(
            !event_text.contains("/Users/xie") && !event_text.contains("ENOENT"),
            "worker report event must not expose raw executor detail: {event_text}"
        );

        let snapshot_text = serde_json::to_string(&runtime.snapshot_for_task(&task_id))
            .expect("task snapshot should serialize");
        assert!(
            !snapshot_text.contains("/Users/xie") && !snapshot_text.contains("ENOENT"),
            "worker report snapshot must not expose raw executor detail: {snapshot_text}"
        );
    }

    #[test]
    fn worker_governance_reason_is_public_in_outcome_snapshot_and_event() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = WorkerRuntime::new_compare(bus.clone());
        let loop_controller = runtime.loop_controller();
        let worker_id = worker_id("worker-governance-public-reason");
        let task_id = task_id("task-governance-public-reason");
        let raw_reason = "/Users/xie/.magi/governance failed: ENOENT";
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
            reason: Some(raw_reason.to_string()),
        };
        let blocked_decision =
            GovernanceService::default().evaluate_worker_control_request(&blocked_request);

        loop_controller.enqueue_guarded_action(
            WorkerLoopAction::Execute {
                worker_id,
                task_id: task_id.clone(),
            },
            Some(blocked_decision),
        );

        let outcome = loop_controller
            .step()
            .expect("blocked outcome should exist");
        assert_eq!(outcome.kind, WorkerLoopOutcomeKind::Blocked);
        assert_eq!(
            outcome.rejection_reason.as_deref(),
            Some("worker 控制动作被治理阻断")
        );
        assert_eq!(
            outcome
                .governance_decision
                .as_ref()
                .and_then(|decision| decision.reason.as_deref()),
            Some("worker 控制动作被治理阻断")
        );

        let event = bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "worker.governance.observed")
            .expect("governance observation event should be published");
        let event_text = event.payload.to_string();
        assert!(event_text.contains("worker 控制动作被治理阻断"));
        assert!(
            !event_text.contains("/Users/xie") && !event_text.contains("ENOENT"),
            "worker governance event must not expose raw reason: {event_text}"
        );

        let snapshot_text = serde_json::to_string(&runtime.snapshot_for_task(&task_id))
            .expect("task snapshot should serialize");
        assert!(
            !snapshot_text.contains("/Users/xie") && !snapshot_text.contains("ENOENT"),
            "worker governance snapshot must not expose raw reason: {snapshot_text}"
        );
    }

    #[test]
    fn worker_governance_allowed_outcome_uses_public_decision_reason() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = WorkerRuntime::new_compare(bus);
        let loop_controller = runtime.loop_controller();
        let worker_id = worker_id("worker-governance-allowed-public-reason");
        let task_id = task_id("task-governance-allowed-public-reason");
        let raw_reason = "/Users/xie/.magi/governance allowed with private context";
        let allowed_request = WorkerControlRequest {
            worker_id: Some(worker_id.clone()),
            mission_id: None,
            assignment_id: None,
            task_id: Some(task_id.clone()),
            action: WorkerControlKind::Execute,
            risk_level: RiskLevel::Low,
            approval_requirement: ApprovalRequirement::None,
            retry_count: 0,
            blocked: false,
            reason: Some(raw_reason.to_string()),
        };
        let allowed_decision =
            GovernanceService::default().evaluate_worker_control_request(&allowed_request);

        loop_controller.enqueue_guarded_action(
            WorkerLoopAction::Execute { worker_id, task_id },
            Some(allowed_decision),
        );

        let outcome = loop_controller
            .step()
            .expect("allowed outcome should exist");
        assert_eq!(outcome.kind, WorkerLoopOutcomeKind::Applied);
        assert_eq!(
            outcome
                .governance_decision
                .as_ref()
                .and_then(|decision| decision.reason.as_deref()),
            Some("worker 控制动作已通过治理检查")
        );
        let outcome_text =
            serde_json::to_string(&outcome).expect("worker outcome should serialize");
        assert!(
            !outcome_text.contains("/Users/xie") && !outcome_text.contains("private context"),
            "worker allowed outcome must not expose raw governance reason: {outcome_text}"
        );
    }

    #[test]
    fn worker_governance_query_output_redacts_existing_raw_reason() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = WorkerRuntime::new_compare(bus);
        let worker_id = worker_id("worker-governance-query-public-reason");
        let task_id = task_id("task-governance-query-public-reason");
        let raw_reason = "/Users/xie/.magi/governance.json parse error";
        runtime
            .governance_observations
            .write()
            .expect("worker governance observation write lock poisoned")
            .push(WorkerGovernanceObservation {
                worker_id: worker_id.clone(),
                task_id: Some(task_id.clone()),
                action: WorkerControlKind::RepairRetry,
                decision: GovernanceDecision::rejected(
                    DecisionPhase::WorkerControl,
                    RiskLevel::Medium,
                    Some(raw_reason.to_string()),
                ),
                observed_at: UtcMillis::now(),
            });

        let observations = runtime.governance_observations();
        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0].decision.reason.as_deref(),
            Some("修复重试不满足执行条件")
        );

        let snapshot_text = serde_json::to_string(&runtime.snapshot_for_worker(&worker_id))
            .expect("worker snapshot should serialize");
        assert!(
            !snapshot_text.contains("/Users/xie") && !snapshot_text.contains("parse error"),
            "worker governance query output must not expose raw reason: {snapshot_text}"
        );
    }

    #[test]
    fn worker_skill_dispatch_query_output_redacts_raw_detail_even_for_existing_records() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = WorkerRuntime::new_compare(bus);
        let worker_id = worker_id("worker-skill-query-public-detail");
        let task_id = task_id("task-skill-query-public-detail");
        let raw_detail = "桥接调用失败[Protocol]: /Users/xie/.mcp/config.json parse error";
        runtime
            .skill_dispatches
            .write()
            .expect("worker skill dispatch write lock poisoned")
            .push(WorkerSkillDispatchObservation {
                worker_id: worker_id.clone(),
                task_id: task_id.clone(),
                tool_call_id: ToolCallId::new("skill-call-query-public-detail"),
                tool_name: "echo.inspect".to_string(),
                route: Some(SkillDispatchRoute::Bridge),
                binding_id: Some("inspect-binding".to_string()),
                status: SkillDispatchStatus::Failed,
                detail: raw_detail.to_string(),
                observed_at: UtcMillis::now(),
            });

        let all_dispatches = runtime.skill_dispatches();
        assert_eq!(all_dispatches.len(), 1);
        assert_eq!(
            all_dispatches[0].detail,
            "Skill 工具调用失败，请检查工具配置或外接服务状态"
        );
        assert!(!all_dispatches[0].detail.contains("/Users/xie"));

        let task_snapshot = runtime.snapshot_for_task(&task_id);
        let event_text = serde_json::to_string(&task_snapshot.skill_dispatches)
            .expect("skill dispatch snapshot should serialize");
        assert!(
            !event_text.contains("/Users/xie") && !event_text.contains("parse error"),
            "worker skill dispatch query output must not expose raw detail: {event_text}"
        );
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
            tool_policy: ToolExecutionPolicy::default(),
            steps: vec![
                WorkerExecutionIntentStep::BuiltinToolInvocation {
                    tool_call_id: ToolCallId::new("call-intent-1".to_string()),
                    tool_name: "file_read".to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: "{\"path\":\"README.md\"}".to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                    status: ExecutionResultStatus::Succeeded,
                },
                WorkerExecutionIntentStep::SkillDispatch {
                    tool_call_id: ToolCallId::new("call-intent-2".to_string()),
                    tool_name: "process_inspect".to_string(),
                    plan: DeterministicWorkerExecutor::default_skill_plan("process_inspect"),
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

        let outcome = loop_controller
            .step()
            .expect("execute outcome should exist");
        assert_eq!(outcome.kind, WorkerLoopOutcomeKind::Applied);
        assert!(outcome.report.is_some());

        let summary = runtime.summary();
        assert_eq!(summary.tool_call_count, 1);
        assert_eq!(summary.skill_dispatch_count, 1);
        assert_eq!(summary.report_count, 1);

        let reports = runtime.reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].summary, "intent completed".to_string());
    }

    #[test]
    fn worker_loop_execute_resumes_from_checkpoint_step_cursor() {
        let bus = Arc::new(InMemoryEventBus::new(16));
        let runtime = WorkerRuntime::new_compare(bus);
        let loop_controller = runtime.loop_controller();
        let worker_id = worker_id("worker-checkpoint");
        let task_id = task_id("todo-checkpoint");

        runtime.register_worker(worker_id.clone());
        runtime.register_execution_intent(WorkerExecutionIntent {
            worker_id: worker_id.clone(),
            task_id: task_id.clone(),
            session_id: None,
            workspace_id: None,
            execution_profile: WorkerExecutionProfile::default(),
            tool_policy: ToolExecutionPolicy::default(),
            steps: vec![
                WorkerExecutionIntentStep::BuiltinToolInvocation {
                    tool_call_id: ToolCallId::new("call-checkpoint-1".to_string()),
                    tool_name: "file_read".to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: "{\"path\":\"README.md\"}".to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                    status: ExecutionResultStatus::Succeeded,
                },
                WorkerExecutionIntentStep::SkillDispatch {
                    tool_call_id: ToolCallId::new("call-checkpoint-2".to_string()),
                    tool_name: "process_inspect".to_string(),
                    plan: DeterministicWorkerExecutor::default_skill_plan("process_inspect"),
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
                    summary: "checkpoint resumed".to_string(),
                    result_kind: Some(TaskResultKind::Success),
                    termination_reason: Some(TerminationReason::Completed),
                    verification_status: VerificationStatus::Passed,
                }),
            ],
        });
        runtime.record_branch_checkpoint(
            &task_id,
            &worker_id,
            WorkerStage::Execute,
            None,
            Some(format!("worker-intent-{task_id}")),
            Some(WorkerExecutionBindingLifecycle::Requested),
            Some(WorkerExecutionCheckpointCursor {
                checkpoint_stage: WorkerStage::Execute,
                next_step_index: 2,
                checkpoint_at: UtcMillis::now(),
                resume_mode: WorkerCheckpointResumeMode::StepCheckpoint,
                resume_token: None,
            }),
        );

        loop_controller.enqueue_action(WorkerLoopAction::Execute {
            worker_id: worker_id.clone(),
            task_id: task_id.clone(),
        });

        let outcome = loop_controller
            .step()
            .expect("execute outcome should exist");
        assert_eq!(outcome.kind, WorkerLoopOutcomeKind::Applied);

        let summary = runtime.summary();
        assert_eq!(summary.tool_call_count, 0);
        assert_eq!(summary.skill_dispatch_count, 0);
        let reports = runtime.reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].summary, "checkpoint resumed");
        let snapshot = runtime
            .branch_snapshot_for_task(&task_id)
            .expect("branch snapshot should remain available");
        assert_eq!(snapshot.stage, WorkerStage::Finish);
        assert!(snapshot.checkpoint_cursor.is_none());
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
        assert!(
            outcome
                .rejection_reason
                .expect("rejection reason missing")
                .contains("executor capability insufficient")
        );
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
        let blocked_decision =
            GovernanceService::default().evaluate_worker_control_request(&blocked_request);
        loop_controller.enqueue_guarded_action(
            WorkerLoopAction::Execute {
                worker_id: worker_id.clone(),
                task_id: task_id.clone(),
            },
            Some(blocked_decision),
        );

        let blocked_outcome = loop_controller
            .step()
            .expect("blocked outcome should exist");
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

        let retry_outcome = loop_controller
            .step()
            .expect("repair retry outcome should exist");
        assert_eq!(retry_outcome.kind, WorkerLoopOutcomeKind::Applied);
        assert!(retry_outcome.report.is_some());
        assert_eq!(runtime.governance_summary().repair_retry, 1);
    }
}
