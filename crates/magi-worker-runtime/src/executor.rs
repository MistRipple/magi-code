use crate::{
    LocalProcessExecutorAffinity, LocalProcessExecutorCapability, LocalProcessExecutorDescriptor,
    LocalProcessExecutorHealth, LocalProcessExecutorHealthStatus,
    LocalProcessExecutorProcessModel, LocalProcessExecutorStageMatrix,
    WorkerExecutionBindingLifecycle, WorkerExecutionBindingScope,
    WorkerExecutionFinalReport, WorkerExecutionIntent, WorkerExecutionIntentStep,
    WorkerExecutionLeaseState, WorkerExecutionMode, WorkerExecutionParallelismScope,
    WorkerExecutionProcessLifecycle, WorkerExecutionProfile, WorkerExecutionStepKind,
    WorkerExecutorFailure, WorkerExecutorRequest, WorkerStage, WorkerToolInvocation,
};
use magi_bridge_client::BridgeBindingDispatchPlan;
use magi_core::{
    ApprovalRequirement, ExecutionResultStatus, RiskLevel, SessionId, TaskResultKind,
    TerminationReason, TaskId, ToolCallId, UtcMillis, VerificationStatus, WorkerId,
    WorkspaceId,
};
use magi_governance::ToolKind;
use magi_skill_runtime::{
    SkillDispatchObservation, SkillDispatchRoute, SkillDispatchStatus,
    SkillToolRoutingSummary, SkillToolRuntimePlan,
};
use magi_tool_runtime::ToolExecutionPolicy;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerExecutionTrace {
    pub worker_id: WorkerId,
    pub task_id: TaskId,
    pub tool_invocations: Vec<WorkerToolInvocation>,
    pub skill_dispatches: Vec<SkillDispatchObservation>,
    pub final_report: WorkerExecutionFinalReport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerExecutorKind {
    Deterministic,
    LocalProcess,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerExecutorFailureDetail {
    pub executor_id: Option<String>,
    pub executor_version: Option<String>,
    pub executor_instance_id: Option<String>,
    pub executor_lease_id: Option<String>,
    pub requested_execution_profile: Option<WorkerExecutionProfile>,
    pub requested_lease_state: Option<WorkerExecutionLeaseState>,
    pub requested_binding_lifecycle: Option<WorkerExecutionBindingLifecycle>,
    pub requested_process_lifecycle: Option<WorkerExecutionProcessLifecycle>,
    pub effective_process_model: Option<LocalProcessExecutorProcessModel>,
    pub effective_lease_state: Option<WorkerExecutionLeaseState>,
    pub effective_binding_lifecycle: Option<WorkerExecutionBindingLifecycle>,
    pub effective_process_lifecycle: Option<WorkerExecutionProcessLifecycle>,
    pub effective_reuse_scope: Option<WorkerExecutionBindingScope>,
    pub effective_parallelism_scope: Option<WorkerExecutionParallelismScope>,
    pub required_step_kinds: Vec<WorkerExecutionStepKind>,
    pub supported_step_kinds: Vec<WorkerExecutionStepKind>,
    pub missing_step_kinds: Vec<WorkerExecutionStepKind>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerExecutorProbe {
    pub executor_id: String,
    pub executor_version: String,
    pub executor_kind: WorkerExecutorKind,
    pub capability: LocalProcessExecutorCapability,
    pub health: LocalProcessExecutorHealth,
}

impl WorkerExecutorProbe {
    pub fn supports_stage(&self, stage: WorkerStage) -> Result<(), WorkerExecutorFailure> {
        if self.capability.supports_stage(stage) {
            Ok(())
        } else {
            Err(WorkerExecutorFailure::remote_business(format!(
                "executor {} {} does not support stage {}",
                self.executor_id,
                self.executor_version,
                stage.label()
            )))
        }
    }

    pub fn supports_context(
        &self,
        session_id: &Option<SessionId>,
        workspace_id: &Option<WorkspaceId>,
    ) -> Result<(), WorkerExecutorFailure> {
        self.capability.supports_context(session_id, workspace_id)
    }

    pub fn supports_execution_profile(
        &self,
        profile: &WorkerExecutionProfile,
    ) -> Result<(), WorkerExecutorFailure> {
        self.capability.supports_profile(profile)
    }

    pub fn supports_request(
        &self,
        request: &WorkerExecutorRequest,
    ) -> Result<(), WorkerExecutorFailure> {
        self.supports_stage(request.requested_stage)?;
        self.supports_context(&request.session_id, &request.workspace_id)?;
        self.supports_execution_profile(&request.requested_execution_profile)?;
        let missing_step_kinds: Vec<_> = request
            .required_step_kinds
            .iter()
            .copied()
            .filter(|kind| !self.capability.supported_step_kinds.contains(kind))
            .collect();
        if missing_step_kinds.is_empty() {
            return Ok(());
        }
        Err(WorkerExecutorFailure::remote_business_with_detail(
            format!(
                "executor {} {} missing required steps: {}",
                self.executor_id,
                self.executor_version,
                missing_step_kinds
                    .iter()
                    .map(WorkerExecutionStepKind::label)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            WorkerExecutorFailureDetail {
                executor_id: Some(self.executor_id.clone()),
                executor_version: Some(self.executor_version.clone()),
                executor_instance_id: self.capability.descriptor.executor_instance_id.clone(),
                executor_lease_id: self.capability.descriptor.executor_lease_id.clone(),
                requested_execution_profile: Some(request.requested_execution_profile.clone()),
                requested_lease_state: Some(request.requested_lease_state),
                requested_binding_lifecycle: Some(request.requested_binding_lifecycle),
                requested_process_lifecycle: Some(request.requested_process_lifecycle),
                effective_process_model: Some(self.capability.descriptor.process_model),
                effective_lease_state: Some(self.capability.descriptor.lease_state),
                effective_binding_lifecycle: Some(self.capability.descriptor.binding_lifecycle),
                effective_process_lifecycle: Some(self.capability.descriptor.process_lifecycle),
                effective_reuse_scope: Some(self.capability.descriptor.reuse_scope),
                effective_parallelism_scope: Some(self.capability.descriptor.parallelism_scope),
                required_step_kinds: request.required_step_kinds.clone(),
                supported_step_kinds: self.capability.supported_step_kinds.clone(),
                missing_step_kinds,
            },
        ))
    }

    pub fn missing_step_kinds(
        &self,
        intent: &WorkerExecutionIntent,
    ) -> Vec<WorkerExecutionStepKind> {
        intent
            .required_step_kinds()
            .into_iter()
            .filter(|kind| !self.capability.supported_step_kinds.contains(kind))
            .collect()
    }

    pub fn supports_intent(
        &self,
        intent: &WorkerExecutionIntent,
    ) -> Result<(), WorkerExecutorFailure> {
        self.supports_request(&intent.executor_request(WorkerStage::Execute, "intent"))
    }
}

pub trait ShadowWorkerExecutor: Send + Sync {
    fn execute(&self, intent: &WorkerExecutionIntent) -> WorkerExecutionTrace;

    fn execute_checked(
        &self,
        intent: &WorkerExecutionIntent,
    ) -> Result<WorkerExecutionTrace, WorkerExecutorFailure> {
        let request = intent.executor_request(WorkerStage::Execute, "execute");
        let probe = self.probe_for_request(Some(&request))?;
        probe.supports_request(&request)?;
        Ok(self.execute(intent))
    }

    fn review(
        &self,
        intent: &WorkerExecutionIntent,
        _prior_trace: Option<&WorkerExecutionTrace>,
    ) -> Result<(WorkerExecutionTrace, String), WorkerExecutorFailure> {
        let request = intent.executor_request(WorkerStage::Review, "review");
        let probe = self.probe_for_request(Some(&request))?;
        probe.supports_request(&request)?;
        let trace = self.execute(intent);
        Ok((trace, "review completed".to_string()))
    }

    fn verify(
        &self,
        intent: &WorkerExecutionIntent,
        _prior_trace: Option<&WorkerExecutionTrace>,
    ) -> Result<(WorkerExecutionTrace, VerificationStatus, String), WorkerExecutorFailure> {
        let request = intent.executor_request(WorkerStage::Verify, "verify");
        let probe = self.probe_for_request(Some(&request))?;
        probe.supports_request(&request)?;
        let trace = self.execute(intent);
        let status = trace.final_report.verification_status;
        Ok((trace, status, "verification completed".to_string()))
    }

    fn repair(
        &self,
        intent: &WorkerExecutionIntent,
        _prior_trace: Option<&WorkerExecutionTrace>,
        repair_reason: &str,
    ) -> Result<(WorkerExecutionTrace, String), WorkerExecutorFailure> {
        let request = intent.executor_request(WorkerStage::Repair, "repair");
        let probe = self.probe_for_request(Some(&request))?;
        probe.supports_request(&request)?;
        let trace = self.execute(intent);
        Ok((trace, format!("repair completed: {repair_reason}")))
    }

    fn probe_for_request(
        &self,
        _request: Option<&WorkerExecutorRequest>,
    ) -> Result<WorkerExecutorProbe, WorkerExecutorFailure> {
        self.probe()
    }

    fn probe(&self) -> Result<WorkerExecutorProbe, WorkerExecutorFailure> {
        Ok(WorkerExecutorProbe {
            executor_id: "shadow-deterministic-worker-executor".to_string(),
            executor_version: "worker-shadow-executor-v1".to_string(),
            executor_kind: self.executor_kind(),
            capability: LocalProcessExecutorCapability {
                executor_id: "shadow-deterministic-worker-executor".to_string(),
                executor_version: "worker-shadow-executor-v1".to_string(),
                protocol_version: "worker-shadow-v1".to_string(),
                execution_mode: WorkerExecutionMode::ShadowLoopback,
                supports_probe: true,
                supports_execute: true,
                supports_review: true,
                supports_verify: true,
                supports_repair: true,
                affinity: LocalProcessExecutorAffinity::default(),
                stage_matrix: LocalProcessExecutorStageMatrix {
                    execute: true,
                    review: true,
                    verify: true,
                    repair: true,
                },
                descriptor: LocalProcessExecutorDescriptor {
                    process_model: LocalProcessExecutorProcessModel::ShadowLoopback,
                    reuse_scope: WorkerExecutionBindingScope::None,
                    parallelism_scope: WorkerExecutionParallelismScope::Executor,
                    lease_state: WorkerExecutionLeaseState::None,
                    binding_lifecycle: WorkerExecutionBindingLifecycle::None,
                    process_lifecycle: WorkerExecutionProcessLifecycle::OneShot,
                    max_parallelism: 1,
                    executor_instance_id: Some(
                        "shadow-deterministic-worker-executor-instance".to_string(),
                    ),
                    executor_lease_id: None,
                },
                supported_step_kinds: vec![
                    WorkerExecutionStepKind::BuiltinToolInvocation,
                    WorkerExecutionStepKind::SkillDispatch,
                    WorkerExecutionStepKind::FinalReport,
                ],
            },
            health: LocalProcessExecutorHealth {
                status: LocalProcessExecutorHealthStatus::Healthy,
                detail: "in-process executor".to_string(),
            },
        })
    }

    fn executor_kind(&self) -> WorkerExecutorKind {
        WorkerExecutorKind::Deterministic
    }
}

#[derive(Clone, Default)]
pub struct DeterministicWorkerExecutor;

impl DeterministicWorkerExecutor {
    pub(crate) fn default_skill_plan(tool_name: &str) -> SkillToolRuntimePlan {
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

    pub fn default_intent(worker_id: WorkerId, task_id: TaskId) -> WorkerExecutionIntent {
        let prefix = format!("shadow-{}-{}", worker_id, task_id);
        WorkerExecutionIntent {
            worker_id,
            task_id,
            session_id: None,
            workspace_id: None,
            execution_profile: WorkerExecutionProfile::default(),
            steps: vec![
                WorkerExecutionIntentStep::BuiltinToolInvocation {
                    tool_call_id: ToolCallId::new(format!("{prefix}-tool-1")),
                    tool_name: "process.inspect".to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: "{\"mode\":\"shadow\",\"target\":\"todo\"}".to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                    status: ExecutionResultStatus::Succeeded,
                },
                WorkerExecutionIntentStep::SkillDispatch {
                    tool_call_id: ToolCallId::new(format!("{prefix}-skill-1")),
                    tool_name: "process.inspect".to_string(),
                    plan: Self::default_skill_plan("process.inspect"),
                    payload: "{\"mode\":\"shadow-skill\",\"target\":\"todo\"}".to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                    working_directory: None,
                    route: SkillDispatchRoute::Builtin,
                    binding_id: None,
                    detail: "shadow skill dispatch".to_string(),
                    status: SkillDispatchStatus::Succeeded,
                },
                WorkerExecutionIntentStep::FinalReport(WorkerExecutionFinalReport {
                    summary: "shadow execution completed".to_string(),
                    result_kind: Some(TaskResultKind::Success),
                    termination_reason: Some(TerminationReason::Completed),
                    verification_status: VerificationStatus::Passed,
                }),
            ],
        }
    }
}

impl ShadowWorkerExecutor for DeterministicWorkerExecutor {
    fn execute(&self, intent: &WorkerExecutionIntent) -> WorkerExecutionTrace {
        let mut tool_invocations = Vec::new();
        let mut skill_dispatches = Vec::new();
        let mut final_report: Option<WorkerExecutionFinalReport> = None;

        for step in &intent.steps {
            match step {
                WorkerExecutionIntentStep::BuiltinToolInvocation {
                    tool_call_id,
                    tool_name,
                    status,
                    ..
                } => {
                    tool_invocations.push(WorkerToolInvocation {
                        worker_id: intent.worker_id.clone(),
                        task_id: intent.task_id.clone(),
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        status: *status,
                        observed_at: UtcMillis::now(),
                    });
                }
                WorkerExecutionIntentStep::SkillDispatch {
                    tool_call_id,
                    tool_name,
                    route,
                    binding_id,
                    detail,
                    status,
                    ..
                } => {
                    skill_dispatches.push(SkillDispatchObservation {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        route: Some(*route),
                        binding_id: binding_id.clone(),
                        bridge_kind: None,
                        dispatch_action: None,
                        status: *status,
                        error_kind: None,
                        bridge_error_layer: None,
                        bridge_error_message: None,
                        detail: detail.clone(),
                    });
                }
                WorkerExecutionIntentStep::FinalReport(report) => {
                    final_report = Some(report.clone());
                }
            }
        }

        let final_report = final_report.unwrap_or_else(|| {
            let mut summary_parts = Vec::new();
            if tool_invocations.iter().any(|record| {
                matches!(
                    record.status,
                    ExecutionResultStatus::Failed | ExecutionResultStatus::Rejected
                )
            }) {
                summary_parts.push("builtin tool step failed");
            }
            if skill_dispatches.iter().any(|record| {
                matches!(
                    record.status,
                    SkillDispatchStatus::Failed | SkillDispatchStatus::Rejected
                )
            }) {
                summary_parts.push("skill dispatch step failed");
            }
            let failed = !summary_parts.is_empty();
            WorkerExecutionFinalReport {
                summary: if failed {
                    summary_parts.join("; ")
                } else {
                    "shadow execution completed".to_string()
                },
                result_kind: Some(if failed {
                    TaskResultKind::Failure
                } else {
                    TaskResultKind::Success
                }),
                termination_reason: Some(if failed {
                    TerminationReason::Failed
                } else {
                    TerminationReason::Completed
                }),
                verification_status: if failed {
                    VerificationStatus::Failed
                } else {
                    VerificationStatus::Passed
                },
            }
        });

        WorkerExecutionTrace {
            worker_id: intent.worker_id.clone(),
            task_id: intent.task_id.clone(),
            tool_invocations,
            skill_dispatches,
            final_report,
        }
    }
}
