
use crate::{
    DispatchExecutionResult, ExecutionContextSummary, ExecutionWritebackPlans,
    OrchestratedExecutionRuntime, OrchestratorCommandError, RecoveryExecutionResult,
    default_builtin_skill_plan, execution_overview, recovery_planner, resolve_skill_tool_name,
};
use magi_core::{
    ExecutionResultStatus, RecoveryResumeInput, SessionId, TaskExecutionTarget, TaskStatus,
    WorkerId, WorkspaceId,
};
use magi_context_runtime::{ExecutionContextAssemblyRequest, ExecutionContextClues};
use magi_event_bus::{EventCategory, EventContext};
use magi_memory_store::MemoryStore;
use magi_skill_runtime::SkillToolRuntimePlan;
use magi_tool_runtime::{ToolExecutionContextQuery, ToolExecutionSummary};
use magi_worker_runtime::{
    TaskExecutionSnapshot, WorkerExecutionIntent, WorkerExecutionReport, WorkerExecutorKind,
    WorkerLoopAction, WorkerSkillDispatchObservation, WorkerStage,
};

impl OrchestratedExecutionRuntime {
    pub fn build_execution_intent(
        &self,
        target: &TaskExecutionTarget,
        worker_id: WorkerId,
        session_id: Option<SessionId>,
        workspace_id: Option<WorkspaceId>,
        skill_plan: Option<SkillToolRuntimePlan>,
    ) -> Option<WorkerExecutionIntent> {
        let task = self.task_store.get_task(&target.task_id)?;
        let prefix = format!(
            "{}-{}-{}",
            target.mission_id, target.root_task_id, target.task_id
        );
        let skill_plan = skill_plan.unwrap_or_else(|| default_builtin_skill_plan("process_inspect"));
        let skill_tool_name = resolve_skill_tool_name(&skill_plan);
        let skill_route = if skill_plan.routing.requested_bridge_tool_names.is_empty()
            && skill_plan.bridge_dispatch_plan.bindings.is_empty()
        {
            magi_skill_runtime::SkillDispatchRoute::Builtin
        } else {
            magi_skill_runtime::SkillDispatchRoute::Bridge
        };
        let skill_binding_id = skill_plan
            .bridge_dispatch_plan
            .bindings
            .first()
            .map(|binding| binding.binding_id.clone());
        let assignment_id = execution_overview::assignment_id_for_task(
            &self.task_store,
            &target.root_task_id,
            &target.task_id,
        );

        Some(WorkerExecutionIntent {
            worker_id,
            task_id: target.task_id.clone(),
            session_id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            execution_profile: self
                .service
                .derive_execution_profile(&session_id, &workspace_id),
            steps: vec![
                magi_worker_runtime::WorkerExecutionIntentStep::BuiltinToolInvocation {
                    tool_call_id: magi_core::ToolCallId::new(format!("{prefix}-builtin-1")),
                    tool_name: "process_inspect".to_string(),
                    tool_kind: magi_governance::ToolKind::Builtin,
                    input: serde_json::json!({
                        "mission_id": target.mission_id.to_string(),
                        "assignment_id": assignment_id.as_ref().map(ToString::to_string),
                        "task_id": target.task_id.to_string(),
                        "task_title": task.title,
                    })
                    .to_string(),
                    approval_requirement: magi_core::ApprovalRequirement::None,
                    risk_level: magi_core::RiskLevel::Low,
                    status: magi_core::ExecutionResultStatus::Succeeded,
                },
                magi_worker_runtime::WorkerExecutionIntentStep::SkillDispatch {
                    tool_call_id: magi_core::ToolCallId::new(format!("{prefix}-skill-1")),
                    tool_name: skill_tool_name,
                    plan: skill_plan,
                    payload: serde_json::json!({
                        "mission_id": target.mission_id.to_string(),
                        "assignment_id": assignment_id.as_ref().map(ToString::to_string),
                        "task_id": target.task_id.to_string(),
                        "task_title": task.title,
                    })
                    .to_string(),
                    approval_requirement: magi_core::ApprovalRequirement::None,
                    risk_level: magi_core::RiskLevel::Low,
                    working_directory: None,
                    route: skill_route,
                    binding_id: skill_binding_id,
                    detail: format!("dispatch execution intent for {}", task.title),
                    status: magi_skill_runtime::SkillDispatchStatus::Succeeded,
                },
                magi_worker_runtime::WorkerExecutionIntentStep::FinalReport(
                    magi_worker_runtime::WorkerExecutionFinalReport {
                        summary: format!("execution intent completed for {}", task.title),
                        result_kind: Some(magi_core::TaskResultKind::Success),
                        termination_reason: Some(magi_core::TerminationReason::Completed),
                        verification_status: magi_core::VerificationStatus::Passed,
                    },
                ),
            ],
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn execute_dispatch(
        &self,
        target: TaskExecutionTarget,
        worker_id: WorkerId,
        session_id: Option<SessionId>,
        workspace_id: Option<WorkspaceId>,
        skill_plan: Option<SkillToolRuntimePlan>,
    ) -> Result<DispatchExecutionResult, OrchestratorCommandError> {
        self.execute_dispatch_flow(target, worker_id, session_id, workspace_id, skill_plan)
    }

    pub fn execute_dispatch_then<F>(
        &self,
        target: TaskExecutionTarget,
        worker_id: WorkerId,
        session_id: Option<SessionId>,
        workspace_id: Option<WorkspaceId>,
        skill_plan: Option<SkillToolRuntimePlan>,
        on_success: F,
    ) -> Result<DispatchExecutionResult, OrchestratorCommandError>
    where
        F: FnOnce(&DispatchExecutionResult),
    {
        let result =
            self.execute_dispatch_flow(target, worker_id, session_id, workspace_id, skill_plan)?;
        on_success(&result);
        Ok(result)
    }

    pub fn execute_dispatch_with_writebacks(
        &self,
        target: TaskExecutionTarget,
        worker_id: WorkerId,
        session_id: Option<SessionId>,
        workspace_id: Option<WorkspaceId>,
        skill_plan: Option<SkillToolRuntimePlan>,
        memory_store: MemoryStore,
        writebacks: ExecutionWritebackPlans,
    ) -> Result<DispatchExecutionResult, OrchestratorCommandError> {
        self.execute_dispatch_then(
            target,
            worker_id,
            session_id,
            workspace_id,
            skill_plan,
            move |_| writebacks.apply(&memory_store),
        )
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn execute_recovery(
        &self,
        input: RecoveryResumeInput,
        worker_id: WorkerId,
        skill_plan: Option<SkillToolRuntimePlan>,
    ) -> Result<RecoveryExecutionResult, OrchestratorCommandError> {
        self.execute_recovery_flow(input, worker_id, skill_plan)
    }

    pub fn execute_recovery_then<F>(
        &self,
        input: RecoveryResumeInput,
        worker_id: WorkerId,
        skill_plan: Option<SkillToolRuntimePlan>,
        on_success: F,
    ) -> Result<RecoveryExecutionResult, OrchestratorCommandError>
    where
        F: FnOnce(&RecoveryExecutionResult),
    {
        let result = self.execute_recovery_flow(input, worker_id, skill_plan)?;
        on_success(&result);
        Ok(result)
    }

    pub fn execute_recovery_with_writebacks(
        &self,
        input: RecoveryResumeInput,
        worker_id: WorkerId,
        skill_plan: Option<SkillToolRuntimePlan>,
        memory_store: MemoryStore,
        writebacks: ExecutionWritebackPlans,
    ) -> Result<RecoveryExecutionResult, OrchestratorCommandError> {
        self.execute_recovery_then(input, worker_id, skill_plan, move |_| {
            writebacks.apply(&memory_store)
        })
    }

    fn execute_recovery_flow(
        &self,
        input: RecoveryResumeInput,
        worker_id: WorkerId,
        skill_plan: Option<SkillToolRuntimePlan>,
    ) -> Result<RecoveryExecutionResult, OrchestratorCommandError> {
        if input.ownership.workspace_id.is_some() {
            let store = self.workspace_store.as_ref().ok_or(
                OrchestratorCommandError::RecoverySupportUnavailable {
                    missing: "workspace_store".to_string(),
                },
            )?;
            store.ensure_recovery_ready(&input.recovery_id).map_err(|error| {
                OrchestratorCommandError::RecoverySupportUnavailable {
                    missing: format!("workspace_store: {error}"),
                }
            })?;
        }
        let resume_command = self
            .service
            .build_resume_command(&input)
            .ok_or(OrchestratorCommandError::NoResumeTarget {
                recovery_id: input.recovery_id.clone(),
            })?;
        let mut target = recovery_planner::build_recovery_target(&self.task_store, &input)
            .ok_or(OrchestratorCommandError::NoResumeTarget {
                recovery_id: input.recovery_id.clone(),
            })?;
        target.requested_worker_id = Some(worker_id.clone());
        let assignment_id = execution_overview::assignment_id_for_task(
            &self.task_store,
            &target.root_task_id,
            &target.task_id,
        );

        let _ = self
            .worker_runtime
            .resume_from_execution_target(&target, worker_id.clone());
        let _ = self.task_store.update_status(&target.task_id, TaskStatus::Running);

        let session_sidecar = if let Some(session_id) = input.ownership.session_id.clone() {
            let store = self.session_store.as_ref().ok_or(
                OrchestratorCommandError::RecoverySupportUnavailable {
                    missing: "session_store".to_string(),
                },
            )?;
            store
                .apply_recovery_resume_input(session_id.clone(), input.clone())
                .map_err(|error| OrchestratorCommandError::RecoverySupportUnavailable {
                    missing: format!("session_store: {error}"),
                })?;
            let session_sidecar = store
                .apply_resume_execution_target(&session_id, &target)
                .map_err(|error| OrchestratorCommandError::RecoverySupportUnavailable {
                    missing: format!("session_store: {error}"),
                })?;
            Some(session_sidecar.export_view())
        } else {
            None
        };

        let workspace_recovery = if let Some(workspace_id) = input.ownership.workspace_id.clone() {
            let store = self.workspace_store.as_ref().ok_or(
                OrchestratorCommandError::RecoverySupportUnavailable {
                    missing: "workspace_store".to_string(),
                },
            )?;
            let resolved_ownership = magi_core::ExecutionOwnership {
                session_id: input.ownership.session_id.clone(),
                workspace_id: Some(workspace_id.clone()),
                mission_id: Some(target.mission_id.clone()),
                task_id: Some(target.task_id.clone()),
                worker_id: target.requested_worker_id.clone(),
                execution_chain_ref: target.execution_chain_ref.clone(),
            };
            let recovery_handle = store
                .consume_recovery_with_ownership(&input.recovery_id, resolved_ownership)
                .map_err(|error| OrchestratorCommandError::RecoverySupportUnavailable {
                    missing: format!("workspace_store: {error}"),
                })?;
            if recovery_handle.workspace_id != workspace_id {
                return Err(OrchestratorCommandError::RecoverySupportUnavailable {
                    missing: format!(
                        "workspace_store/recovery workspace mismatch: {} != {}",
                        recovery_handle.workspace_id, workspace_id
                    ),
                });
            }
            Some(recovery_handle.export_view())
        } else {
            None
        };

        let dispatch = self.execute_dispatch_flow(
            target.clone(),
            worker_id,
            input.ownership.session_id.clone(),
            input.ownership.workspace_id.clone(),
            skill_plan,
        )?;
        let runtime_snapshot = dispatch.overview.runtime_snapshot.clone();
        Ok(RecoveryExecutionResult {
            recovery_input: input,
            resume_command,
            target,
            assignment_id,
            dispatch,
            session_sidecar,
            workspace_recovery,
            runtime_snapshot,
        })
    }

    fn build_context_summary_for_dispatch(
        &self,
        target: &TaskExecutionTarget,
        session_id: &Option<SessionId>,
        workspace_id: &Option<WorkspaceId>,
    ) -> Option<ExecutionContextSummary> {
        let context_runtime = self.context_runtime.as_ref()?;
        let context_config = self.context_config.as_ref()?;
        let session_id = session_id.clone()?;
        let workspace_id = workspace_id.clone()?;
        let descriptor = self.dispatch_context_descriptor(target)?;

        let request = ExecutionContextAssemblyRequest {
            session_id,
            workspace_id,
            project_key: context_config.project_key.clone(),
            clues: ExecutionContextClues {
                mission: descriptor.mission_title,
                assignment: descriptor.assignment_title,
                task: descriptor.task_title,
            },
            budget: context_config.budget.clone(),
        };

        Some(ExecutionContextSummary::from_context_assembly(
            &context_runtime.assemble_execution_context(&request),
        ))
    }

    fn execute_dispatch_flow(
        &self,
        target: TaskExecutionTarget,
        worker_id: WorkerId,
        session_id: Option<SessionId>,
        workspace_id: Option<WorkspaceId>,
        skill_plan: Option<SkillToolRuntimePlan>,
    ) -> Result<DispatchExecutionResult, OrchestratorCommandError> {
        let executor_probe = if matches!(
            self.worker_runtime.executor_kind(),
            WorkerExecutorKind::LocalProcess
        ) {
            let probe = self
                .worker_runtime
                .executor_probe()
                .map_err(|error| OrchestratorCommandError::WorkerExecutorUnavailable {
                    reason: error.to_string(),
                })?;
            if !probe.capability.supports_execute {
                return Err(OrchestratorCommandError::WorkerExecutorUnavailable {
                    reason: "local process executor does not support execute".to_string(),
                });
            }
            if probe.health.status
                != magi_worker_runtime::LocalProcessExecutorHealthStatus::Healthy
            {
                return Err(OrchestratorCommandError::WorkerExecutorUnavailable {
                    reason: format!(
                        "local process executor is not healthy: {}",
                        probe.health.detail
                    ),
                });
            }
            probe
                .supports_context(&session_id, &workspace_id)
                .map_err(|error| OrchestratorCommandError::WorkerExecutorUnavailable {
                    reason: error.to_string(),
                })?;
            Some(probe)
        } else {
            None
        };
        let mut intent = self
            .build_execution_intent(
                &target,
                worker_id.clone(),
                session_id.clone(),
                workspace_id.clone(),
                skill_plan,
            )
            .ok_or(OrchestratorCommandError::TaskNotFound {
                task_id: target.task_id.clone(),
            })?;
        let mut executor_request = self
            .service
            .derive_executor_request(&intent, "dispatch");
        if let Some(probe) = &executor_probe {
            intent.execution_profile =
                self.service.finalize_execution_profile(&intent.execution_profile, probe);
            executor_request.requested_execution_profile = intent.execution_profile.clone();
            probe
                .supports_execution_profile(&intent.execution_profile)
                .map_err(|error| OrchestratorCommandError::WorkerExecutorUnavailable {
                    reason: error.to_string(),
                })?;
            probe.supports_request(&executor_request).map_err(|error| {
                OrchestratorCommandError::WorkerExecutorUnavailable {
                    reason: error.to_string(),
                }
            })?;
        }
        self.worker_runtime.register_execution_intent(intent.clone());
        let loop_controller = match self.worker_runtime.executor_kind() {
            WorkerExecutorKind::LocalProcess => self.worker_runtime.loop_controller(),
            WorkerExecutorKind::Deterministic => self
                .worker_runtime
                .loop_controller()
                .with_execution_drivers(
                    self.tool_registry.clone(),
                    self.skill_dispatch_runtime.clone(),
                ),
        };
        loop_controller.enqueue_action(WorkerLoopAction::Execute {
            worker_id,
            task_id: target.task_id.clone(),
        });
        let outcome = loop_controller
            .step()
            .ok_or(OrchestratorCommandError::NoDispatchTarget {
                mission_id: target.mission_id.clone(),
            })?;
        if let Some(report) = outcome.report.clone() {
            self.apply_worker_report(&target, &report)?;
        }
        let snapshot = self.worker_runtime.snapshot_for_task(&target.task_id);
        for observation in &snapshot.skill_dispatches {
            self.publish_worker_skill_dispatch_observation(&target, observation);
        }
        let tool_summary = match self.worker_runtime.executor_kind() {
            WorkerExecutorKind::LocalProcess => tool_summary_from_worker_snapshot(&snapshot),
            WorkerExecutorKind::Deterministic => {
                self.tool_registry.summary_for_query(&ToolExecutionContextQuery {
                    task_id: Some(target.task_id.clone()),
                    ..ToolExecutionContextQuery::default()
                })
            }
        };
        let context_summary =
            self.build_context_summary_for_dispatch(&target, &session_id, &workspace_id);
        let overview = self
            .service
            .build_execution_overview_from_task_graph(
                &self.task_store,
                &target,
                self.worker_runtime.summary(),
                tool_summary,
                &snapshot.skill_dispatches,
                &snapshot.governance_observations,
                context_summary,
            )
            .ok_or(OrchestratorCommandError::MissionNotFound {
                mission_id: target.mission_id.clone(),
            })?;
        Ok(DispatchExecutionResult {
            target,
            intent,
            outcome,
            overview,
        })
    }

    fn dispatch_context_descriptor(
        &self,
        target: &TaskExecutionTarget,
    ) -> Option<crate::DispatchContextDescriptor> {
        let root_task = self.task_store.get_task(&target.root_task_id)?;
        let task = self.task_store.get_task(&target.task_id)?;
        Some(crate::DispatchContextDescriptor {
            mission_title: Some(root_task.title),
            assignment_title: execution_overview::assignment_title_for_task(
                &self.task_store,
                &target.root_task_id,
                &target.task_id,
            ),
            task_title: Some(task.title),
        })
    }

    fn apply_worker_report(
        &self,
        target: &TaskExecutionTarget,
        report: &WorkerExecutionReport,
    ) -> Result<(), OrchestratorCommandError> {
        let next_status = match report.termination_reason {
            Some(magi_core::TerminationReason::Completed) => TaskStatus::Completed,
            Some(magi_core::TerminationReason::Failed) => TaskStatus::Failed,
            Some(magi_core::TerminationReason::Blocked) => TaskStatus::Blocked,
            Some(magi_core::TerminationReason::Cancelled) => TaskStatus::Cancelled,
            None => match report.stage {
                WorkerStage::Execute
                | WorkerStage::Review
                | WorkerStage::Verify
                | WorkerStage::Repair => TaskStatus::Running,
                WorkerStage::Finish => {
                    if report.result_kind == Some(magi_core::TaskResultKind::Success)
                        && report.verification_status != magi_core::VerificationStatus::Failed
                    {
                        TaskStatus::Completed
                    } else {
                        TaskStatus::Failed
                    }
                }
            },
        };
        self.task_store
            .update_status(&report.task_id, next_status)
            .map_err(|_| OrchestratorCommandError::TaskNotFound {
                task_id: report.task_id.clone(),
            })?;
        let assignment_id = execution_overview::assignment_id_for_task(
            &self.task_store,
            &target.root_task_id,
            &report.task_id,
        );
        self.service.publish_with_category(
            "worker.report.applied",
            EventCategory::Domain,
            EventContext {
                mission_id: Some(target.mission_id.clone()),
                assignment_id: assignment_id.clone(),
                task_id: Some(report.task_id.clone()),
                ..EventContext::default()
            },
            serde_json::json!({
                "worker_id": report.worker_id.to_string(),
                "task_id": report.task_id.to_string(),
                "mission_id": target.mission_id.to_string(),
                "assignment_id": assignment_id.as_ref().map(ToString::to_string),
                "status": format!("{:?}", next_status),
                "stage": format!("{:?}", report.stage),
                "termination_reason": report.termination_reason.map(|value| format!("{:?}", value)),
                "verification_status": format!("{:?}", report.verification_status)
            }),
        );
        Ok(())
    }

    fn publish_worker_skill_dispatch_observation(
        &self,
        target: &TaskExecutionTarget,
        observation: &WorkerSkillDispatchObservation,
    ) {
        let assignment_id = execution_overview::assignment_id_for_task(
            &self.task_store,
            &target.root_task_id,
            &observation.task_id,
        );
        self.service.publish_with_category(
            "worker.skill_dispatch.applied",
            EventCategory::Audit,
            EventContext {
                mission_id: Some(target.mission_id.clone()),
                assignment_id: assignment_id.clone(),
                task_id: Some(observation.task_id.clone()),
                ..EventContext::default()
            },
            serde_json::json!({
                "worker_id": observation.worker_id.to_string(),
                "task_id": observation.task_id.to_string(),
                "mission_id": target.mission_id.to_string(),
                "assignment_id": assignment_id.as_ref().map(ToString::to_string),
                "tool_call_id": observation.tool_call_id.to_string(),
                "tool_name": observation.tool_name,
                "route": observation.route.map(|route| format!("{:?}", route)),
                "binding_id": observation.binding_id,
                "status": format!("{:?}", observation.status)
            }),
        );
    }

}

fn tool_summary_from_worker_snapshot(snapshot: &TaskExecutionSnapshot) -> ToolExecutionSummary {
    let mut summary = ToolExecutionSummary::default();
    for invocation in &snapshot.tool_invocations {
        summary.total_invocations += 1;
        match invocation.status {
            ExecutionResultStatus::Succeeded => summary.successful_invocations += 1,
            ExecutionResultStatus::Failed | ExecutionResultStatus::Cancelled => {
                summary.failed_invocations += 1
            }
            ExecutionResultStatus::Rejected | ExecutionResultStatus::NeedsApproval => {
                summary.blocked_invocations += 1
            }
        }
    }
    summary
}
