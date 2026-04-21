#![recursion_limit = "256"]

mod control_plane;
pub mod auto_learning;
pub mod dispatch;
mod execution_overview;
mod execution_runtime;
mod execution_writeback;
pub mod plan_ledger;
mod recovery_planner;
pub mod risk_policy;
pub mod task_runner;
pub mod task_store;
pub mod task_worker_catalog;
pub mod verification_policy;
pub mod verification_runner;

use magi_core::{
    ApprovalRequirement, AssignmentId, AssignmentLifecycleStatus, DispatchReason, EventId, MissionId,
    MissionLifecycleStatus, RecoveryResumeInput, ResumeDispatchDecision, TaskResultKind,
    RiskLevel, SessionId, TaskId, TaskStatus, TerminationReason, ToolCallId, UtcMillis,
    VerificationStatus,
    WorkerId, WorkspaceId,
};
use magi_bridge_client::BridgeBindingDispatchPlan;
use magi_context_runtime::{ContextAssemblyResult, ContextBudget, ContextRuntime};
use magi_event_bus::{EventCategory, EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::{
    GovernanceDecision, GovernanceOutcome, GovernanceService, WorkerControlKind,
    WorkerControlRequest, ToolKind,
};
use magi_session_store::{SessionRuntimeSidecarExport, SessionStore};
use magi_skill_runtime::{
    SkillDispatchRoute, SkillDispatchRuntime, SkillDispatchStatus, SkillToolRoutingSummary,
    SkillToolRuntimePlan,
};
use magi_tool_runtime::{ToolExecutionPolicy, ToolExecutionSummary, ToolRegistry};
use magi_workspace::{WorkspaceRecoverySidecarExport, WorkspaceStore};
use magi_worker_runtime::{
    SkillDispatchSummary, WorkerExecutionFinalReport, WorkerExecutionIntent,
    WorkerExecutionBindingScope, WorkerExecutionIntentStep, WorkerExecutionProfile,
    WorkerExecutorRequest,
    WorkerExecutionReport, WorkerExecutionReusePolicy, WorkerGovernanceObservation,
    WorkerGovernanceSummary, WorkerLoopOutcome, WorkerRuntime, WorkerRuntimeSummary,
    WorkerSkillDispatchObservation, WorkerStage,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

pub use execution_writeback::{DispatchMemoryExtractionInput, ExecutionWritebackPlans};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: TaskId,
    pub title: String,
    pub status: TaskStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssignmentRecord {
    pub assignment_id: AssignmentId,
    pub title: String,
    pub status: AssignmentLifecycleStatus,
    pub tasks: Vec<TaskRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissionRecord {
    pub mission_id: MissionId,
    pub title: String,
    pub status: MissionLifecycleStatus,
    pub created_at: UtcMillis,
    pub assignments: Vec<AssignmentRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DispatchDecision {
    pub mission_id: MissionId,
    pub assignment_id: AssignmentId,
    pub task_id: TaskId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum OrchestratorCommand {
    CreateMission {
        mission_id: MissionId,
        title: String,
    },
    AddAssignment {
        mission_id: MissionId,
        assignment_id: AssignmentId,
        title: String,
    },
    CreateTask {
        mission_id: MissionId,
        assignment_id: AssignmentId,
        task_id: TaskId,
        title: String,
    },
    DispatchNextTask {
        mission_id: MissionId,
    },
    ApplyWorkerReport {
        report: WorkerExecutionReport,
    },
    ApplyWorkerSkillDispatchObservation {
        observation: WorkerSkillDispatchObservation,
    },
    ApplyGovernanceDecision {
        request: WorkerControlRequest,
    },
    BuildMissionExecutionOverview {
        mission_id: MissionId,
        worker_summary: WorkerRuntimeSummary,
        tool_summary: ToolExecutionSummary,
        skill_dispatch_observations: Vec<WorkerSkillDispatchObservation>,
        governance_observations: Vec<WorkerGovernanceObservation>,
        context_summary: Option<MissionContextSummary>,
    },
    BuildResumeCommand {
        input: RecoveryResumeInput,
    },
    BuildResumeDispatchDecision {
        input: RecoveryResumeInput,
    },
    ResumeFromRecovery {
        input: RecoveryResumeInput,
    },
    ResumeFromDispatchDecision {
        decision: ResumeDispatchDecision,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum OrchestratorCommandResult {
    MissionCreated {
        mission: MissionRecord,
    },
    AssignmentAdded {
        mission: MissionRecord,
    },
    TaskCreated {
        mission: MissionRecord,
    },
    TaskDispatchPlanned {
        decision: DispatchDecision,
    },
    WorkerReportApplied {
        mission: MissionRecord,
    },
    WorkerSkillDispatchObservationApplied {
        snapshot: MissionRuntimeSnapshot,
    },
    GovernanceDecisionApplied {
        mission: MissionRecord,
        decision: GovernanceDecision,
        disposition: GovernanceDisposition,
    },
    MissionExecutionOverviewBuilt {
        overview: MissionExecutionOverview,
    },
    ResumeCommandBuilt {
        command: ResumeCommand,
    },
    ResumeDispatchDecisionBuilt {
        decision: ResumeDispatchDecision,
    },
    MissionResumed {
        mission: MissionRecord,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum OrchestratorCommandError {
    MissionNotFound {
        mission_id: MissionId,
    },
    AssignmentNotFound {
        mission_id: MissionId,
        assignment_id: AssignmentId,
    },
    TaskNotFound {
        task_id: TaskId,
    },
    NoDispatchTarget {
        mission_id: MissionId,
    },
    GovernanceTargetMissing {
        reason: String,
    },
    NoResumeTarget {
        recovery_id: String,
    },
    RecoverySupportUnavailable {
        missing: String,
    },
    WorkerExecutorUnavailable {
        reason: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissionRuntimeSnapshot {
    pub mission_id: MissionId,
    pub total_assignments: usize,
    pub total_tasks: usize,
    pub completed_tasks: usize,
    pub failed_tasks: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissionExecutionOverview {
    pub mission: MissionRuntimeSnapshot,
    pub running_task_ids: Vec<TaskId>,
    pub worker_summary: WorkerRuntimeSummary,
    pub tool_summary: ToolExecutionSummary,
    pub governance_summary: WorkerGovernanceSummary,
    pub skill_dispatch_summary: MissionSkillDispatchSummary,
    pub context_summary: Option<MissionContextSummary>,
    pub assignment_governance_summaries: Vec<AssignmentGovernanceSummary>,
    pub task_governance_summaries: Vec<TaskGovernanceSummary>,
    pub assignment_skill_dispatch_summaries: Vec<AssignmentSkillDispatchSummary>,
    pub task_skill_dispatch_summaries: Vec<TaskSkillDispatchSummary>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MissionContextSummary {
    pub used_turns: usize,
    pub used_knowledge: usize,
    pub used_memory: usize,
    pub used_shared_items: usize,
    pub used_file_summaries: usize,
    pub truncation_count: usize,
    pub truncation_parts: Vec<String>,
    pub knowledge_ids: Vec<String>,
    pub knowledge_source_paths: Vec<String>,
    pub memory_ids: Vec<String>,
    pub memory_extraction_refs: Vec<String>,
    pub code_index_knowledge_count: usize,
    pub audited_knowledge_count: usize,
    pub governed_knowledge_count: usize,
    pub extracted_memory_count: usize,
    pub provenance_linked_memory_count: usize,
}

impl MissionContextSummary {
    pub fn from_context_assembly(result: &ContextAssemblyResult) -> Self {
        let mut truncation_parts = result
            .usage
            .truncations
            .iter()
            .map(|record| record.part.clone())
            .collect::<Vec<_>>();
        truncation_parts.sort();
        truncation_parts.dedup();

        let mut knowledge_ids = result
            .selected_knowledge
            .iter()
            .map(|record| record.knowledge_id.clone())
            .collect::<Vec<_>>();
        knowledge_ids.sort();
        knowledge_ids.dedup();

        let mut knowledge_source_paths = result
            .selected_knowledge
            .iter()
            .filter_map(|record| record.code_source.as_ref().map(|source| source.path.clone()))
            .collect::<Vec<_>>();
        knowledge_source_paths.sort();
        knowledge_source_paths.dedup();

        let mut memory_ids = result
            .selected_memory
            .iter()
            .map(|record| record.memory_id.clone())
            .collect::<Vec<_>>();
        memory_ids.sort();
        memory_ids.dedup();

        let mut memory_extraction_refs = result
            .selected_memory
            .iter()
            .filter_map(|record| {
                record
                    .provenance
                    .as_ref()
                    .and_then(|provenance| provenance.extracted_from.clone())
            })
            .collect::<Vec<_>>();
        memory_extraction_refs.sort();
        memory_extraction_refs.dedup();

        Self {
            used_turns: result.usage.used_turns,
            used_knowledge: result.usage.used_knowledge,
            used_memory: result.usage.used_memory,
            used_shared_items: result.usage.used_shared_items,
            used_file_summaries: result.usage.used_file_summaries,
            truncation_count: result.usage.truncations.len(),
            truncation_parts,
            knowledge_ids,
            knowledge_source_paths,
            memory_ids,
            memory_extraction_refs,
            code_index_knowledge_count: result
                .selected_knowledge
                .iter()
                .filter(|record| record.code_source.is_some())
                .count(),
            audited_knowledge_count: result
                .selected_knowledge
                .iter()
                .filter(|record| record.audit_link.is_some())
                .count(),
            governed_knowledge_count: result
                .selected_knowledge
                .iter()
                .filter(|record| record.governance_link.is_some())
                .count(),
            extracted_memory_count: result
                .selected_memory
                .iter()
                .filter(|record| {
                    record.provenance.as_ref().is_some_and(|provenance| {
                        provenance.source.eq_ignore_ascii_case("extraction")
                            || provenance.extracted_from.is_some()
                    })
                })
                .count(),
            provenance_linked_memory_count: result
                .selected_memory
                .iter()
                .filter(|record| record.provenance.is_some())
                .count(),
        }
    }
}

pub type MissionSkillDispatchSummary = SkillDispatchSummary;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssignmentGovernanceSummary {
    pub assignment_id: AssignmentId,
    pub mission_id: MissionId,
    pub governance_summary: WorkerGovernanceSummary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskGovernanceSummary {
    pub task_id: TaskId,
    pub mission_id: MissionId,
    pub assignment_id: AssignmentId,
    pub governance_summary: WorkerGovernanceSummary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssignmentSkillDispatchSummary {
    pub assignment_id: AssignmentId,
    pub mission_id: MissionId,
    pub skill_dispatch_summary: MissionSkillDispatchSummary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskSkillDispatchSummary {
    pub task_id: TaskId,
    pub mission_id: MissionId,
    pub assignment_id: AssignmentId,
    pub skill_dispatch_summary: MissionSkillDispatchSummary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResumeCommand {
    pub mission_id: MissionId,
    pub task_id: Option<TaskId>,
    pub dispatch_reason: DispatchReason,
    pub recovery_id: String,
    pub execution_chain_ref: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernanceDisposition {
    Allowed,
    NeedsApproval,
    Rejected,
    Blocked,
    RepairRetryScheduled,
}

#[derive(Clone, Debug, Default)]
pub struct OrchestratorState {
    pub missions: HashMap<MissionId, MissionRecord>,
}

#[derive(Clone)]
pub struct OrchestratorService {
    state: Arc<RwLock<OrchestratorState>>,
    event_bus: Arc<InMemoryEventBus>,
    governance: Arc<GovernanceService>,
}

#[derive(Clone)]
pub struct OrchestratorControlPlane {
    service: OrchestratorService,
}

#[derive(Clone)]
pub struct OrchestratedExecutionRuntime {
    service: OrchestratorService,
    worker_runtime: WorkerRuntime,
    tool_registry: ToolRegistry,
    skill_dispatch_runtime: SkillDispatchRuntime,
    session_store: Option<Arc<SessionStore>>,
    workspace_store: Option<Arc<WorkspaceStore>>,
    context_runtime: Option<ContextRuntime>,
    context_config: Option<ExecutionContextConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionContextConfig {
    pub budget: ContextBudget,
    pub project_key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DispatchExecutionResult {
    pub decision: DispatchDecision,
    pub intent: WorkerExecutionIntent,
    pub outcome: WorkerLoopOutcome,
    pub overview: MissionExecutionOverview,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryExecutionResult {
    pub recovery_input: RecoveryResumeInput,
    pub resume_command: ResumeCommand,
    pub decision: ResumeDispatchDecision,
    pub dispatch: DispatchExecutionResult,
    pub session_sidecar: Option<SessionRuntimeSidecarExport>,
    pub workspace_recovery: Option<WorkspaceRecoverySidecarExport>,
    pub mission_snapshot: MissionRuntimeSnapshot,
}

impl OrchestratorService {
    pub fn new(event_bus: Arc<InMemoryEventBus>) -> Self {
        Self {
            state: Arc::new(RwLock::new(OrchestratorState::default())),
            event_bus,
            governance: Arc::new(GovernanceService::default()),
        }
    }

    pub fn with_governance(
        event_bus: Arc<InMemoryEventBus>,
        governance: Arc<GovernanceService>,
    ) -> Self {
        Self {
            state: Arc::new(RwLock::new(OrchestratorState::default())),
            event_bus,
            governance,
        }
    }

    pub fn control_plane(&self) -> OrchestratorControlPlane {
        OrchestratorControlPlane {
            service: self.clone(),
        }
    }

    pub fn execution_runtime(
        &self,
        worker_runtime: WorkerRuntime,
        tool_registry: ToolRegistry,
        skill_dispatch_runtime: SkillDispatchRuntime,
    ) -> OrchestratedExecutionRuntime {
        OrchestratedExecutionRuntime {
            service: self.clone(),
            worker_runtime,
            tool_registry,
            skill_dispatch_runtime,
            session_store: None,
            workspace_store: None,
            context_runtime: None,
            context_config: None,
        }
    }

    pub fn execution_runtime_with_recovery_support(
        &self,
        worker_runtime: WorkerRuntime,
        tool_registry: ToolRegistry,
        skill_dispatch_runtime: SkillDispatchRuntime,
        session_store: Arc<SessionStore>,
        workspace_store: Arc<WorkspaceStore>,
    ) -> OrchestratedExecutionRuntime {
        OrchestratedExecutionRuntime {
            service: self.clone(),
            worker_runtime,
            tool_registry,
            skill_dispatch_runtime,
            session_store: Some(session_store),
            workspace_store: Some(workspace_store),
            context_runtime: None,
            context_config: None,
        }
    }

    pub fn create_mission(&self, mission_id: MissionId, title: impl Into<String>) -> MissionRecord {
        let mission = MissionRecord {
            mission_id: mission_id.clone(),
            title: title.into(),
            status: MissionLifecycleStatus::Pending,
            created_at: UtcMillis::now(),
            assignments: Vec::new(),
        };
        self.state
            .write()
            .expect("orchestrator state write lock poisoned")
            .missions
            .insert(mission_id.clone(), mission.clone());
        self.publish("mission.created", serde_json::json!({
            "mission_id": mission_id.to_string()
        }));
        mission
    }

    pub fn add_assignment(
        &self,
        mission_id: &MissionId,
        assignment_id: AssignmentId,
        title: impl Into<String>,
    ) -> Option<MissionRecord> {
        let mut state = self
            .state
            .write()
            .expect("orchestrator state write lock poisoned");
        let mission = state.missions.get_mut(mission_id)?;
        mission.assignments.push(AssignmentRecord {
            assignment_id: assignment_id.clone(),
            title: title.into(),
            status: AssignmentLifecycleStatus::Pending,
            tasks: Vec::new(),
        });
        let mission = mission.clone();
        drop(state);
        self.publish("assignment.created", serde_json::json!({
            "mission_id": mission_id.to_string(),
            "assignment_id": assignment_id.to_string()
        }));
        Some(mission)
    }

    pub fn create_task(
        &self,
        mission_id: &MissionId,
        assignment_id: &AssignmentId,
        task_id: TaskId,
        title: impl Into<String>,
    ) -> Option<MissionRecord> {
        let mut state = self
            .state
            .write()
            .expect("orchestrator state write lock poisoned");
        let mission = state.missions.get_mut(mission_id)?;
        let assignment = mission
            .assignments
            .iter_mut()
            .find(|assignment| &assignment.assignment_id == assignment_id)?;
        assignment.tasks.push(TaskRecord {
            task_id: task_id.clone(),
            title: title.into(),
            status: TaskStatus::Ready,
        });
        let mission = mission.clone();
        drop(state);
        self.publish("task.created", serde_json::json!({
            "mission_id": mission_id.to_string(),
            "assignment_id": assignment_id.to_string(),
            "task_id": task_id.to_string()
        }));
        Some(mission)
    }

    pub fn dispatch_next_task(&self, mission_id: &MissionId) -> Option<DispatchDecision> {
        let mut state = self
            .state
            .write()
            .expect("orchestrator state write lock poisoned");
        let mission = state.missions.get_mut(mission_id)?;
        mission.status = MissionLifecycleStatus::Running;

        for assignment in &mut mission.assignments {
            if assignment.status == AssignmentLifecycleStatus::Pending {
                assignment.status = AssignmentLifecycleStatus::Running;
            }
            for task in &mut assignment.tasks {
                if task.status == TaskStatus::Ready {
                    task.status = TaskStatus::Running;
                    let decision = DispatchDecision {
                        mission_id: mission_id.clone(),
                        assignment_id: assignment.assignment_id.clone(),
                        task_id: task.task_id.clone(),
                    };
                    self.publish("task.dispatched", serde_json::json!({
                        "mission_id": decision.mission_id.to_string(),
                        "assignment_id": decision.assignment_id.to_string(),
                        "task_id": decision.task_id.to_string()
                    }));
                    return Some(decision);
                }
            }
        }
        None
    }

    pub fn build_execution_intent(
        &self,
        decision: &DispatchDecision,
        worker_id: WorkerId,
        session_id: Option<SessionId>,
        workspace_id: Option<WorkspaceId>,
        skill_plan: Option<SkillToolRuntimePlan>,
    ) -> Option<WorkerExecutionIntent> {
        let outcome = self.find_task_status_outcome(&decision.task_id)?;
        let task = outcome
            .mission
            .assignments
            .iter()
            .find(|assignment| assignment.assignment_id == decision.assignment_id)?
            .tasks
            .iter()
            .find(|task| task.task_id == decision.task_id)?;
        let prefix = format!(
            "{}-{}-{}",
            decision.mission_id, decision.assignment_id, decision.task_id
        );
        let skill_plan = skill_plan.unwrap_or_else(|| default_builtin_skill_plan("process.inspect"));
        let skill_tool_name = resolve_skill_tool_name(&skill_plan);
        let skill_route = if skill_plan.routing.requested_bridge_tool_names.is_empty()
            && skill_plan.bridge_dispatch_plan.bindings.is_empty()
        {
            SkillDispatchRoute::Builtin
        } else {
            SkillDispatchRoute::Bridge
        };
        let skill_binding_id = skill_plan
            .bridge_dispatch_plan
            .bindings
            .first()
            .map(|binding| binding.binding_id.clone());

        Some(WorkerExecutionIntent {
            worker_id,
            task_id: decision.task_id.clone(),
            session_id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            execution_profile: self.derive_execution_profile(&session_id, &workspace_id),
            steps: vec![
                WorkerExecutionIntentStep::BuiltinToolInvocation {
                    tool_call_id: ToolCallId::new(format!("{prefix}-builtin-1")),
                    tool_name: "process.inspect".to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: serde_json::json!({
                        "mission_id": decision.mission_id.to_string(),
                        "assignment_id": decision.assignment_id.to_string(),
                        "task_id": decision.task_id.to_string(),
                        "task_title": task.title,
                    })
                    .to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                    status: magi_core::ExecutionResultStatus::Succeeded,
                },
                WorkerExecutionIntentStep::SkillDispatch {
                    tool_call_id: ToolCallId::new(format!("{prefix}-skill-1")),
                    tool_name: skill_tool_name,
                    plan: skill_plan,
                    payload: serde_json::json!({
                        "mission_id": decision.mission_id.to_string(),
                        "assignment_id": decision.assignment_id.to_string(),
                        "task_id": decision.task_id.to_string(),
                        "task_title": task.title,
                    })
                    .to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                    working_directory: None,
                    route: skill_route,
                    binding_id: skill_binding_id,
                    detail: format!("dispatch execution intent for {}", task.title),
                    status: SkillDispatchStatus::Succeeded,
                },
                WorkerExecutionIntentStep::FinalReport(WorkerExecutionFinalReport {
                    summary: format!("execution intent completed for {}", task.title),
                    result_kind: Some(TaskResultKind::Success),
                    termination_reason: Some(TerminationReason::Completed),
                    verification_status: VerificationStatus::Passed,
                }),
            ],
        })
    }

    pub(crate) fn dispatch_context_descriptor(
        &self,
        decision: &DispatchDecision,
    ) -> Option<DispatchContextDescriptor> {
        let state = self
            .state
            .read()
            .expect("orchestrator state read lock poisoned");
        let mission = state.missions.get(&decision.mission_id)?;
        let assignment = mission
            .assignments
            .iter()
            .find(|assignment| assignment.assignment_id == decision.assignment_id)?;
        let task = assignment
            .tasks
            .iter()
            .find(|task| task.task_id == decision.task_id)?;

        Some(DispatchContextDescriptor {
            mission_title: Some(mission.title.clone()),
            assignment_title: Some(assignment.title.clone()),
            task_title: Some(task.title.clone()),
        })
    }

    fn derive_execution_profile(
        &self,
        session_id: &Option<SessionId>,
        workspace_id: &Option<WorkspaceId>,
    ) -> WorkerExecutionProfile {
        let binding_scope = if workspace_id.is_some() {
            WorkerExecutionBindingScope::Workspace
        } else if session_id.is_some() {
            WorkerExecutionBindingScope::Session
        } else {
            WorkerExecutionBindingScope::None
        };
        WorkerExecutionProfile {
            reuse_policy: if binding_scope == WorkerExecutionBindingScope::None {
                WorkerExecutionReusePolicy::NotRequired
            } else {
                WorkerExecutionReusePolicy::Preferred
            },
            binding_scope,
            lease_state: if binding_scope == WorkerExecutionBindingScope::None {
                magi_worker_runtime::WorkerExecutionLeaseState::None
            } else {
                magi_worker_runtime::WorkerExecutionLeaseState::Requested
            },
            binding_lifecycle: if binding_scope == WorkerExecutionBindingScope::None {
                magi_worker_runtime::WorkerExecutionBindingLifecycle::None
            } else {
                magi_worker_runtime::WorkerExecutionBindingLifecycle::Requested
            },
            process_lifecycle: magi_worker_runtime::WorkerExecutionProcessLifecycle::OneShot,
            requested_process_model: None,
            requested_parallelism: 1,
        }
    }

    pub(crate) fn finalize_execution_profile(
        &self,
        profile: &WorkerExecutionProfile,
        probe: &magi_worker_runtime::WorkerExecutorProbe,
    ) -> WorkerExecutionProfile {
        let mut effective_profile = profile.clone();
        if effective_profile.requested_process_model.is_none() {
            effective_profile.requested_process_model =
                Some(probe.capability.descriptor.process_model);
        }
        effective_profile.process_lifecycle = match probe.capability.descriptor.process_model {
            magi_worker_runtime::LocalProcessExecutorProcessModel::PersistentProcess => {
                magi_worker_runtime::WorkerExecutionProcessLifecycle::Persistent
            }
            _ => magi_worker_runtime::WorkerExecutionProcessLifecycle::OneShot,
        };
        if probe.capability.descriptor.reuse_scope != magi_worker_runtime::WorkerExecutionBindingScope::None {
            effective_profile.binding_lifecycle = magi_worker_runtime::WorkerExecutionBindingLifecycle::Bound;
            effective_profile.lease_state = magi_worker_runtime::WorkerExecutionLeaseState::Active;
        }
        effective_profile
    }

    pub(crate) fn derive_executor_request(
        &self,
        intent: &WorkerExecutionIntent,
        request_source: &str,
    ) -> WorkerExecutorRequest {
        intent.executor_request(WorkerStage::Execute, request_source.to_string())
    }

    pub fn complete_task(
        &self,
        mission_id: &MissionId,
        assignment_id: &AssignmentId,
        task_id: &TaskId,
    ) -> Option<MissionRecord> {
        self.update_task_status(
            mission_id,
            assignment_id,
            task_id,
            TaskStatus::Completed,
            "task.completed",
        )
    }

    pub fn fail_task(
        &self,
        mission_id: &MissionId,
        assignment_id: &AssignmentId,
        task_id: &TaskId,
    ) -> Option<MissionRecord> {
        self.update_task_status(
            mission_id,
            assignment_id,
            task_id,
            TaskStatus::Failed,
            "task.failed",
        )
    }

    pub fn missions(&self) -> Vec<MissionRecord> {
        self.state
            .read()
            .expect("orchestrator state read lock poisoned")
            .missions
            .values()
            .cloned()
            .collect()
    }

    pub fn mission_runtime_snapshot(
        &self,
        mission_id: &MissionId,
    ) -> Option<MissionRuntimeSnapshot> {
        let state = self
            .state
            .read()
            .expect("orchestrator state read lock poisoned");
        let mission = state.missions.get(mission_id)?;
        Some(execution_overview::build_runtime_snapshot(mission))
    }

    pub fn apply_worker_report(&self, report: &WorkerExecutionReport) -> Option<MissionRecord> {
        let next_status = match report.termination_reason {
            Some(TerminationReason::Completed) => TaskStatus::Completed,
            Some(TerminationReason::Failed) => TaskStatus::Failed,
            Some(TerminationReason::Blocked) => TaskStatus::Blocked,
            Some(TerminationReason::Cancelled) => TaskStatus::Cancelled,
            None => match report.stage {
                WorkerStage::Execute
                | WorkerStage::Review
                | WorkerStage::Verify
                | WorkerStage::Repair => TaskStatus::Running,
                WorkerStage::Finish => {
                    if report.result_kind == Some(TaskResultKind::Success)
                        && report.verification_status != VerificationStatus::Failed
                    {
                        TaskStatus::Completed
                    } else {
                        TaskStatus::Failed
                    }
                }
            },
        };

        let outcome = self.update_task_status_by_task_id(&report.task_id, next_status)?;
        self.publish_with_category(
            "worker.report.applied",
            EventCategory::Domain,
            EventContext {
                mission_id: Some(outcome.mission_id.clone()),
                assignment_id: Some(outcome.assignment_id.clone()),
                task_id: Some(report.task_id.clone()),
                ..EventContext::default()
            },
            serde_json::json!({
                "worker_id": report.worker_id.to_string(),
                "task_id": report.task_id.to_string(),
                "mission_id": outcome.mission_id.to_string(),
                "assignment_id": outcome.assignment_id.to_string(),
                "status": format!("{:?}", next_status),
                "stage": format!("{:?}", report.stage),
                "termination_reason": report.termination_reason.map(|value| format!("{:?}", value)),
                "verification_status": format!("{:?}", report.verification_status)
            }),
        );
        Some(outcome.mission)
    }

    pub fn apply_worker_runtime_summary(
        &self,
        summary: &WorkerRuntimeSummary,
        mission_id: &MissionId,
    ) -> Option<MissionRuntimeSnapshot> {
        let snapshot = self.mission_runtime_snapshot(mission_id)?;
        self.publish_with_category(
            "worker.summary.observed",
            EventCategory::Audit,
            EventContext {
                mission_id: Some(mission_id.clone()),
                ..EventContext::default()
            },
            serde_json::json!({
                "mission_id": mission_id.to_string(),
                "worker_total": summary.total_workers,
                "worker_active": summary.active_workers,
                "worker_finished": summary.finished_workers,
                "worker_failed": summary.failed_workers,
                "report_count": summary.report_count,
                "skill_dispatch_count": summary.skill_dispatch_count,
                "skill_dispatch_builtin": summary.skill_dispatch_summary.builtin_dispatches,
                "skill_dispatch_bridge": summary.skill_dispatch_summary.bridge_dispatches,
                "skill_dispatch_succeeded": summary.skill_dispatch_summary.succeeded_dispatches,
                "skill_dispatch_rejected": summary.skill_dispatch_summary.rejected_dispatches,
                "skill_dispatch_failed": summary.skill_dispatch_summary.failed_dispatches,
                "task_total": snapshot.total_tasks,
                "task_completed": snapshot.completed_tasks,
                "task_failed": snapshot.failed_tasks
            }),
        );
        Some(snapshot)
    }

    pub fn apply_worker_skill_dispatch_observation(
        &self,
        observation: &WorkerSkillDispatchObservation,
    ) -> Option<MissionRuntimeSnapshot> {
        let outcome = self.find_task_status_outcome(&observation.task_id)?;
        let snapshot = execution_overview::build_runtime_snapshot(&outcome.mission);
        self.publish_with_category(
            "worker.skill_dispatch.applied",
            EventCategory::Audit,
            EventContext {
                mission_id: Some(outcome.mission_id.clone()),
                assignment_id: Some(outcome.assignment_id.clone()),
                task_id: Some(observation.task_id.clone()),
                ..EventContext::default()
            },
            serde_json::json!({
                "worker_id": observation.worker_id.to_string(),
                "task_id": observation.task_id.to_string(),
                "mission_id": outcome.mission_id.to_string(),
                "assignment_id": outcome.assignment_id.to_string(),
                "tool_call_id": observation.tool_call_id.to_string(),
                "tool_name": observation.tool_name,
                "route": observation.route.map(|route| format!("{:?}", route)),
                "binding_id": observation.binding_id,
                "status": format!("{:?}", observation.status)
            }),
        );
        Some(snapshot)
    }

    pub fn apply_governance_decision(
        &self,
        request: &WorkerControlRequest,
    ) -> Result<(MissionRecord, GovernanceDecision, GovernanceDisposition), OrchestratorCommandError>
    {
        let mission_id = request
            .mission_id
            .as_ref()
            .ok_or_else(|| OrchestratorCommandError::GovernanceTargetMissing {
                reason: "缺少 mission_id".to_string(),
            })?;
        let assignment_id = request
            .assignment_id
            .as_ref()
            .ok_or_else(|| OrchestratorCommandError::GovernanceTargetMissing {
                reason: "缺少 assignment_id".to_string(),
            })?;
        let task_id = request
            .task_id
            .as_ref()
            .ok_or_else(|| OrchestratorCommandError::GovernanceTargetMissing {
                reason: "缺少 task_id".to_string(),
            })?;

        if !self.mission_exists(mission_id) {
            return Err(OrchestratorCommandError::MissionNotFound {
                mission_id: mission_id.clone(),
            });
        }
        if !self.assignment_exists(mission_id, assignment_id) {
            return Err(OrchestratorCommandError::AssignmentNotFound {
                mission_id: mission_id.clone(),
                assignment_id: assignment_id.clone(),
            });
        }

        let decision = self.governance.evaluate_worker_control_request(request);
        let disposition = self.governance_disposition(&decision, &request.action);
        let mission = match disposition {
            GovernanceDisposition::Allowed => {
                self.mark_task_progress(
                    mission_id,
                    assignment_id,
                    task_id,
                    TaskStatus::Running,
                    "task.governance.allowed",
                )
            }
            GovernanceDisposition::RepairRetryScheduled => {
                self.mark_task_progress(
                    mission_id,
                    assignment_id,
                    task_id,
                    TaskStatus::Running,
                    "task.governance.repair_retry",
                )
            }
            GovernanceDisposition::NeedsApproval => {
                self.mark_task_progress(
                    mission_id,
                    assignment_id,
                    task_id,
                    TaskStatus::Blocked,
                    "task.governance.approval_required",
                )
            }
            GovernanceDisposition::Blocked => {
                self.mark_task_progress(
                    mission_id,
                    assignment_id,
                    task_id,
                    TaskStatus::Blocked,
                    "task.governance.blocked",
                )
            }
            GovernanceDisposition::Rejected => self.update_task_status(
                mission_id,
                assignment_id,
                task_id,
                TaskStatus::Failed,
                "task.governance.rejected",
            ),
        }
        .ok_or_else(|| OrchestratorCommandError::TaskNotFound {
            task_id: task_id.clone(),
        })?;

        self.publish_with_category(
            "governance.decision.applied",
            EventCategory::Audit,
            EventContext {
                mission_id: Some(mission_id.clone()),
                assignment_id: Some(assignment_id.clone()),
                task_id: Some(task_id.clone()),
                ..EventContext::default()
            },
            serde_json::json!({
                "mission_id": mission_id.to_string(),
                "assignment_id": assignment_id.to_string(),
                "task_id": task_id.to_string(),
                "worker_id": request.worker_id.as_ref().map(ToString::to_string),
                "action": format!("{:?}", request.action),
                "outcome": format!("{:?}", decision.outcome),
                "disposition": format!("{:?}", disposition),
                "status": self.governance_status_label(&decision, &disposition),
                "reason": decision.reason,
            }),
        );

        Ok((mission, decision, disposition))
    }

    pub fn build_execution_overview(
        &self,
        mission_id: &MissionId,
        worker_summary: WorkerRuntimeSummary,
        tool_summary: ToolExecutionSummary,
        skill_dispatch_observations: &[WorkerSkillDispatchObservation],
        governance_observations: &[WorkerGovernanceObservation],
    ) -> Option<MissionExecutionOverview> {
        self.build_execution_overview_with_context(
            mission_id,
            worker_summary,
            tool_summary,
            skill_dispatch_observations,
            governance_observations,
            None,
        )
    }

    pub fn build_execution_overview_with_context(
        &self,
        mission_id: &MissionId,
        worker_summary: WorkerRuntimeSummary,
        tool_summary: ToolExecutionSummary,
        skill_dispatch_observations: &[WorkerSkillDispatchObservation],
        governance_observations: &[WorkerGovernanceObservation],
        context_summary: Option<MissionContextSummary>,
    ) -> Option<MissionExecutionOverview> {
        let state = self
            .state
            .read()
            .expect("orchestrator state read lock poisoned");
        let mission = state.missions.get(mission_id)?;
        let overview = execution_overview::build_execution_overview(
            mission,
            worker_summary,
            tool_summary,
            skill_dispatch_observations,
            governance_observations,
            context_summary,
        );
        drop(state);
        self.publish_with_category(
            "mission.execution.overview",
            EventCategory::Audit,
            EventContext {
                mission_id: Some(overview.mission.mission_id.clone()),
                ..EventContext::default()
            },
            execution_overview::build_execution_overview_payload(&overview),
        );
        Some(overview)
    }

    pub fn build_resume_command(&self, input: &RecoveryResumeInput) -> Option<ResumeCommand> {
        let command = recovery_planner::build_resume_command(input)?;
        self.publish_with_category(
            "mission.resume.command.created",
            EventCategory::Audit,
            EventContext {
                workspace_id: input.ownership.workspace_id.clone(),
                session_id: input.ownership.session_id.clone(),
                mission_id: Some(command.mission_id.clone()),
                task_id: command.task_id.clone(),
                ..EventContext::default()
            },
            recovery_planner::build_resume_command_payload(&command),
        );
        Some(command)
    }

    pub fn build_resume_dispatch_decision(
        &self,
        input: &RecoveryResumeInput,
    ) -> Option<ResumeDispatchDecision> {
        let mission_id = input.ownership.mission_id.as_ref()?;
        let state = self
            .state
            .read()
            .expect("orchestrator state read lock poisoned");
        let mission = state.missions.get(mission_id)?;
        let decision = recovery_planner::build_resume_dispatch_decision(mission, input)?;
        drop(state);
        self.publish_with_category(
            "mission.resume.dispatch.created",
            EventCategory::Domain,
            EventContext {
                workspace_id: input.ownership.workspace_id.clone(),
                session_id: input.ownership.session_id.clone(),
                mission_id: Some(decision.mission_id.clone()),
                assignment_id: Some(decision.assignment_id.clone()),
                task_id: Some(decision.task_id.clone()),
            },
            recovery_planner::build_resume_dispatch_payload(&decision),
        );
        Some(decision)
    }

    pub fn resume_from_recovery(&self, input: &RecoveryResumeInput) -> Option<MissionRecord> {
        let decision = self.build_resume_dispatch_decision(input)?;
        self.resume_from_dispatch_decision(&decision)
    }

    pub fn resume_from_dispatch_decision(
        &self,
        decision: &ResumeDispatchDecision,
    ) -> Option<MissionRecord> {
        let mut state = self
            .state
            .write()
            .expect("orchestrator state write lock poisoned");
        let mission = state.missions.get_mut(&decision.mission_id)?;
        mission.status = MissionLifecycleStatus::Running;
        for assignment in &mut mission.assignments {
            if assignment.assignment_id == decision.assignment_id {
                assignment.status = AssignmentLifecycleStatus::Running;
                for task in &mut assignment.tasks {
                    if task.task_id == decision.task_id {
                        task.status = TaskStatus::Running;
                    }
                }
            }
        }
        let mission_snapshot = mission.clone();
        drop(state);
        self.publish_with_category(
            "mission.resumed.from_recovery",
            EventCategory::Domain,
            EventContext {
                mission_id: Some(mission_snapshot.mission_id.clone()),
                assignment_id: Some(decision.assignment_id.clone()),
                task_id: Some(decision.task_id.clone()),
                ..EventContext::default()
            },
            recovery_planner::build_resume_outcome_payload(decision),
        );
        Some(mission_snapshot)
    }

    pub(crate) fn mission_exists(&self, mission_id: &MissionId) -> bool {
        self.state
            .read()
            .expect("orchestrator state read lock poisoned")
            .missions
            .contains_key(mission_id)
    }

    pub(crate) fn assignment_exists(
        &self,
        mission_id: &MissionId,
        assignment_id: &AssignmentId,
    ) -> bool {
        self.state
            .read()
            .expect("orchestrator state read lock poisoned")
            .missions
            .get(mission_id)
            .is_some_and(|mission| {
                mission
                    .assignments
                    .iter()
                    .any(|assignment| &assignment.assignment_id == assignment_id)
            })
    }

    fn update_task_status(
        &self,
        mission_id: &MissionId,
        assignment_id: &AssignmentId,
        task_id: &TaskId,
        next_status: TaskStatus,
        event_type: &str,
    ) -> Option<MissionRecord> {
        let mut state = self
            .state
            .write()
            .expect("orchestrator state write lock poisoned");
        let mission = state.missions.get_mut(mission_id)?;
        let assignment = mission
            .assignments
            .iter_mut()
            .find(|assignment| &assignment.assignment_id == assignment_id)?;
        let task = assignment
            .tasks
            .iter_mut()
            .find(|task| &task.task_id == task_id)?;
        task.status = next_status;
        synchronize_assignment_status(assignment);
        synchronize_mission_status(mission);

        let mission_snapshot = mission.clone();
        drop(state);
        self.publish(event_type, serde_json::json!({
            "mission_id": mission_id.to_string(),
            "assignment_id": assignment_id.to_string(),
            "task_id": task_id.to_string(),
            "status": format!("{:?}", next_status)
        }));
        Some(mission_snapshot)
    }

    fn mark_task_progress(
        &self,
        mission_id: &MissionId,
        assignment_id: &AssignmentId,
        task_id: &TaskId,
        next_status: TaskStatus,
        event_type: &str,
    ) -> Option<MissionRecord> {
        let mut state = self
            .state
            .write()
            .expect("orchestrator state write lock poisoned");
        let mission = state.missions.get_mut(mission_id)?;
        let assignment = mission
            .assignments
            .iter_mut()
            .find(|assignment| &assignment.assignment_id == assignment_id)?;
        let task = assignment
            .tasks
            .iter_mut()
            .find(|task| &task.task_id == task_id)?;
        task.status = next_status;

        if assignment.tasks.iter().all(|task| {
            matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Cancelled
            )
        }) {
            assignment.status = AssignmentLifecycleStatus::Succeeded;
        } else if assignment
            .tasks
            .iter()
            .any(|task| task.status == TaskStatus::Failed)
        {
            assignment.status = AssignmentLifecycleStatus::Failed;
        } else {
            assignment.status = AssignmentLifecycleStatus::Running;
        }

        if mission
            .assignments
            .iter()
            .all(|assignment| assignment.status == AssignmentLifecycleStatus::Succeeded)
        {
            mission.status = MissionLifecycleStatus::Succeeded;
        } else if mission
            .assignments
            .iter()
            .any(|assignment| assignment.status == AssignmentLifecycleStatus::Failed)
        {
            mission.status = MissionLifecycleStatus::Failed;
        } else {
            mission.status = MissionLifecycleStatus::Running;
        }

        let mission_snapshot = mission.clone();
        drop(state);
        self.publish(event_type, serde_json::json!({
            "mission_id": mission_id.to_string(),
            "assignment_id": assignment_id.to_string(),
            "task_id": task_id.to_string(),
            "status": format!("{:?}", next_status)
        }));
        Some(mission_snapshot)
    }

    fn update_task_status_by_task_id(
        &self,
        task_id: &TaskId,
        next_status: TaskStatus,
    ) -> Option<TaskStatusOutcome> {
        let mut state = self
            .state
            .write()
            .expect("orchestrator state write lock poisoned");
        for mission in state.missions.values_mut() {
            for assignment_index in 0..mission.assignments.len() {
                let task_position = mission.assignments[assignment_index]
                    .tasks
                    .iter()
                    .position(|task| &task.task_id == task_id);
                if let Some(task_index) = task_position {
                    mission.assignments[assignment_index].tasks[task_index].status = next_status;

                    let assignment_status = if mission.assignments[assignment_index].tasks.iter().all(
                        |task| {
                            matches!(
                                task.status,
                                TaskStatus::Completed | TaskStatus::Cancelled
                            )
                        },
                    ) {
                        AssignmentLifecycleStatus::Succeeded
                    } else if mission.assignments[assignment_index]
                        .tasks
                        .iter()
                        .any(|task| {
                            matches!(
                                task.status,
                                TaskStatus::Failed | TaskStatus::Blocked
                            )
                        })
                    {
                        AssignmentLifecycleStatus::Failed
                    } else {
                        AssignmentLifecycleStatus::Running
                    };
                    mission.assignments[assignment_index].status = assignment_status;

                    mission.status = if mission
                        .assignments
                        .iter()
                        .all(|assignment| assignment.status == AssignmentLifecycleStatus::Succeeded)
                    {
                        MissionLifecycleStatus::Succeeded
                    } else if mission
                        .assignments
                        .iter()
                        .any(|assignment| assignment.status == AssignmentLifecycleStatus::Failed)
                    {
                        MissionLifecycleStatus::Failed
                    } else {
                        MissionLifecycleStatus::Running
                    };

                    let outcome = TaskStatusOutcome {
                        mission_id: mission.mission_id.clone(),
                        assignment_id: mission.assignments[assignment_index]
                            .assignment_id
                            .clone(),
                        mission: mission.clone(),
                    };
                    return Some(outcome);
                }
            }
        }
        None
    }

    fn governance_disposition(
        &self,
        decision: &GovernanceDecision,
        action: &WorkerControlKind,
    ) -> GovernanceDisposition {
        match decision.outcome {
            GovernanceOutcome::Allowed => {
                if matches!(action, WorkerControlKind::RepairRetry) {
                    GovernanceDisposition::RepairRetryScheduled
                } else {
                    GovernanceDisposition::Allowed
                }
            }
            GovernanceOutcome::NeedsApproval => GovernanceDisposition::NeedsApproval,
            GovernanceOutcome::Rejected => GovernanceDisposition::Rejected,
            GovernanceOutcome::Blocked => GovernanceDisposition::Blocked,
        }
    }

    fn governance_status_label(
        &self,
        decision: &GovernanceDecision,
        disposition: &GovernanceDisposition,
    ) -> &'static str {
        match (decision.outcome, disposition) {
            (GovernanceOutcome::Allowed, GovernanceDisposition::RepairRetryScheduled) => "running",
            (GovernanceOutcome::Allowed, _) => "running",
            (GovernanceOutcome::NeedsApproval, _) => "needs_approval",
            (GovernanceOutcome::Rejected, _) => "failed",
            (GovernanceOutcome::Blocked, _) => "blocked",
        }
    }

    fn find_task_status_outcome(&self, task_id: &TaskId) -> Option<TaskStatusOutcome> {
        let state = self
            .state
            .read()
            .expect("orchestrator state read lock poisoned");
        for mission in state.missions.values() {
            for assignment in &mission.assignments {
                if assignment.tasks.iter().any(|task| &task.task_id == task_id) {
                    return Some(TaskStatusOutcome {
                        mission_id: mission.mission_id.clone(),
                        assignment_id: assignment.assignment_id.clone(),
                        mission: mission.clone(),
                    });
                }
            }
        }
        None
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

impl OrchestratedExecutionRuntime {
    pub fn with_context_runtime(
        mut self,
        context_runtime: ContextRuntime,
        context_config: ExecutionContextConfig,
    ) -> Self {
        self.context_runtime = Some(context_runtime);
        self.context_config = Some(context_config);
        self
    }
}

struct TaskStatusOutcome {
    mission_id: MissionId,
    assignment_id: AssignmentId,
    mission: MissionRecord,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DispatchContextDescriptor {
    pub mission_title: Option<String>,
    pub assignment_title: Option<String>,
    pub task_title: Option<String>,
}

fn default_builtin_skill_plan(tool_name: &str) -> SkillToolRuntimePlan {
    SkillToolRuntimePlan {
        skill_ids: vec!["shadow-skill".to_string()],
        tool_policy: ToolExecutionPolicy::default(),
        routing: SkillToolRoutingSummary {
            requested_builtin_tools: vec![tool_name.to_string()],
            requested_bridge_tool_names: Vec::new(),
            requested_bridge_binding_ids: Vec::new(),
            denied_requested_tools: Vec::new(),
        },
        prompt_injections: Vec::new(),
        custom_tool_bindings: Vec::new(),
        bridge_dispatch_plan: BridgeBindingDispatchPlan {
            source_skill_ids: vec!["shadow-skill".to_string()],
            bindings: Vec::new(),
        },
    }
}

fn resolve_skill_tool_name(plan: &SkillToolRuntimePlan) -> String {
    plan.routing
        .requested_bridge_tool_names
        .first()
        .cloned()
        .or_else(|| {
            plan.bridge_dispatch_plan
                .bindings
                .first()
                .map(|binding| binding.tool_name.clone())
        })
        .or_else(|| plan.routing.requested_builtin_tools.first().cloned())
        .unwrap_or_else(|| "process.inspect".to_string())
}

fn synchronize_assignment_status(assignment: &mut AssignmentRecord) {
    assignment.status = if assignment.tasks.iter().all(|task| {
        matches!(
            task.status,
            TaskStatus::Completed | TaskStatus::Cancelled
        )
    }) {
        AssignmentLifecycleStatus::Succeeded
    } else if assignment
        .tasks
        .iter()
        .any(|task| matches!(task.status, TaskStatus::Failed))
    {
        AssignmentLifecycleStatus::Failed
    } else if assignment.tasks.iter().any(|task| {
        matches!(
            task.status,
            TaskStatus::Running | TaskStatus::Blocked
        )
    }) {
        AssignmentLifecycleStatus::Running
    } else {
        AssignmentLifecycleStatus::Pending
    };
}

fn synchronize_mission_status(mission: &mut MissionRecord) {
    mission.status = if mission.assignments.iter().all(|assignment| {
        matches!(
            assignment.status,
            AssignmentLifecycleStatus::Succeeded | AssignmentLifecycleStatus::Cancelled
        )
    }) {
        MissionLifecycleStatus::Succeeded
    } else if mission
        .assignments
        .iter()
        .any(|assignment| matches!(assignment.status, AssignmentLifecycleStatus::Failed))
    {
        MissionLifecycleStatus::Failed
    } else if mission
        .assignments
        .iter()
        .any(|assignment| matches!(assignment.status, AssignmentLifecycleStatus::Running))
    {
        MissionLifecycleStatus::Running
    } else {
        MissionLifecycleStatus::Pending
    };
}

#[cfg(test)]
mod tests;
