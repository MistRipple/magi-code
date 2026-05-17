#![recursion_limit = "256"]

pub mod auto_learning;
mod execution_overview;
mod execution_runtime;
mod execution_writeback;
#[cfg(test)]
pub(crate) mod plan_ledger;
pub mod risk_policy;
pub mod task_store;
pub mod task_worker_catalog;
pub mod turn_diff_tracker;
pub mod verification_policy;
pub mod verification_runner;

use magi_bridge_client::BridgeBindingDispatchPlan;
use magi_context_runtime::{ContextAssemblyResult, ContextBudget, ContextRuntime};
use magi_core::{
    AssignmentId, EventId, MissionId, SessionId, TaskExecutionTarget, TaskId, UtcMillis,
    WorkspaceId,
};
use magi_event_bus::{EventCategory, EventContext, EventEnvelope, InMemoryEventBus};
use magi_skill_runtime::{SkillDispatchRuntime, SkillToolRoutingSummary, SkillToolRuntimePlan};
use magi_tool_runtime::{ToolExecutionPolicy, ToolExecutionSummary, ToolRegistry};
use magi_worker_runtime::{
    SkillDispatchSummary, WorkerExecutionBindingScope, WorkerExecutionIntent,
    WorkerExecutionProfile, WorkerExecutionReusePolicy, WorkerExecutorRequest,
    WorkerGovernanceObservation, WorkerGovernanceSummary, WorkerLoopOutcome, WorkerRuntime,
    WorkerRuntimeSummary, WorkerSkillDispatchObservation, WorkerStage,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub use execution_writeback::{DispatchMemoryExtractionInput, ExecutionWritebackPlans};

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
    TaskStateViolation {
        task_id: TaskId,
        message: String,
    },
    NoDispatchTarget {
        mission_id: MissionId,
    },
    GovernanceTargetMissing {
        reason: String,
    },
    WorkerExecutorUnavailable {
        reason: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionRuntimeSnapshot {
    pub mission_id: MissionId,
    pub total_assignments: usize,
    pub total_tasks: usize,
    pub completed_tasks: usize,
    pub failed_tasks: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionOverview {
    pub runtime_snapshot: ExecutionRuntimeSnapshot,
    pub running_task_ids: Vec<TaskId>,
    pub worker_summary: WorkerRuntimeSummary,
    pub tool_summary: ToolExecutionSummary,
    pub governance_summary: WorkerGovernanceSummary,
    pub skill_dispatch_summary: ExecutionSkillDispatchSummary,
    pub context_summary: Option<ExecutionContextSummary>,
    pub assignment_governance_summaries: Vec<AssignmentGovernanceSummary>,
    pub task_governance_summaries: Vec<TaskGovernanceSummary>,
    pub assignment_skill_dispatch_summaries: Vec<AssignmentSkillDispatchSummary>,
    pub task_skill_dispatch_summaries: Vec<TaskSkillDispatchSummary>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExecutionContextSummary {
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

impl ExecutionContextSummary {
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
            .filter_map(|record| {
                record
                    .code_source
                    .as_ref()
                    .map(|source| source.path.clone())
            })
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

pub type ExecutionSkillDispatchSummary = SkillDispatchSummary;

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
    pub skill_dispatch_summary: ExecutionSkillDispatchSummary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskSkillDispatchSummary {
    pub task_id: TaskId,
    pub mission_id: MissionId,
    pub assignment_id: AssignmentId,
    pub skill_dispatch_summary: ExecutionSkillDispatchSummary,
}

#[derive(Clone)]
pub struct OrchestratorService {
    event_bus: Arc<InMemoryEventBus>,
}

#[derive(Clone)]
pub struct OrchestratedExecutionRuntime {
    service: OrchestratorService,
    task_store: Arc<task_store::TaskStore>,
    worker_runtime: WorkerRuntime,
    tool_registry: ToolRegistry,
    skill_dispatch_runtime: SkillDispatchRuntime,
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
    pub target: TaskExecutionTarget,
    pub intent: WorkerExecutionIntent,
    pub outcome: WorkerLoopOutcome,
    pub overview: ExecutionOverview,
}

impl OrchestratorService {
    pub fn new(event_bus: Arc<InMemoryEventBus>) -> Self {
        Self { event_bus }
    }

    pub fn execution_runtime(
        &self,
        worker_runtime: WorkerRuntime,
        tool_registry: ToolRegistry,
        skill_dispatch_runtime: SkillDispatchRuntime,
    ) -> OrchestratedExecutionRuntime {
        OrchestratedExecutionRuntime {
            service: self.clone(),
            task_store: Arc::new(task_store::TaskStore::new()),
            worker_runtime,
            tool_registry,
            skill_dispatch_runtime,
            context_runtime: None,
            context_config: None,
        }
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
        if probe.capability.descriptor.reuse_scope
            != magi_worker_runtime::WorkerExecutionBindingScope::None
        {
            effective_profile.binding_lifecycle =
                magi_worker_runtime::WorkerExecutionBindingLifecycle::Bound;
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

    pub(crate) fn build_execution_overview_from_task_projection(
        &self,
        task_store: &task_store::TaskStore,
        target: &TaskExecutionTarget,
        worker_summary: WorkerRuntimeSummary,
        tool_summary: ToolExecutionSummary,
        skill_dispatch_observations: &[WorkerSkillDispatchObservation],
        governance_observations: &[WorkerGovernanceObservation],
        context_summary: Option<ExecutionContextSummary>,
    ) -> Option<ExecutionOverview> {
        let overview = execution_overview::build_execution_overview_from_task_projection(
            task_store,
            &target.root_task_id,
            &target.mission_id,
            worker_summary,
            tool_summary,
            skill_dispatch_observations,
            governance_observations,
            context_summary,
        )?;
        self.publish_with_category(
            "mission.execution.overview",
            EventCategory::Audit,
            EventContext {
                mission_id: Some(overview.runtime_snapshot.mission_id.clone()),
                ..EventContext::default()
            },
            execution_overview::build_execution_overview_payload(&overview),
        );
        Some(overview)
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
    pub fn worker_runtime(&self) -> &WorkerRuntime {
        &self.worker_runtime
    }

    pub fn task_store(&self) -> &task_store::TaskStore {
        &self.task_store
    }
}

impl OrchestratedExecutionRuntime {
    pub fn with_task_store(mut self, task_store: Arc<task_store::TaskStore>) -> Self {
        self.task_store = task_store;
        self
    }

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

#[derive(Clone, Debug, Default)]
pub(crate) struct DispatchContextDescriptor {
    pub mission_title: Option<String>,
    pub assignment_title: Option<String>,
    pub task_title: Option<String>,
}

fn default_builtin_skill_plan(tool_name: &str) -> SkillToolRuntimePlan {
    SkillToolRuntimePlan {
        skill_ids: vec!["test-skill".to_string()],
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
            source_skill_ids: vec!["test-skill".to_string()],
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
        .unwrap_or_else(|| "process_inspect".to_string())
}

#[cfg(test)]
mod tests;
