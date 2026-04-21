
use crate::{
    DispatchDecision, DispatchExecutionResult, MissionContextSummary,
    ExecutionWritebackPlans, OrchestratedExecutionRuntime, OrchestratorCommandError,
    RecoveryExecutionResult,
};
use magi_core::{
    ExecutionResultStatus, RecoveryResumeInput, SessionId, WorkerId, WorkspaceId,
};
use magi_context_runtime::{ExecutionContextAssemblyRequest, ExecutionContextClues};
use magi_memory_store::MemoryStore;
use magi_skill_runtime::SkillToolRuntimePlan;
use magi_tool_runtime::{ToolExecutionContextQuery, ToolExecutionSummary};
use magi_worker_runtime::{
    TaskExecutionSnapshot, WorkerExecutionIntent, WorkerExecutorKind, WorkerLoopAction,
};

impl OrchestratedExecutionRuntime {
    pub fn build_execution_intent(
        &self,
        decision: &DispatchDecision,
        worker_id: WorkerId,
        session_id: Option<SessionId>,
        workspace_id: Option<WorkspaceId>,
        skill_plan: Option<SkillToolRuntimePlan>,
    ) -> Option<WorkerExecutionIntent> {
        self.service.build_execution_intent(
            decision,
            worker_id,
            session_id,
            workspace_id,
            skill_plan,
        )
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn execute_dispatch(
        &self,
        decision: DispatchDecision,
        worker_id: WorkerId,
        session_id: Option<SessionId>,
        workspace_id: Option<WorkspaceId>,
        skill_plan: Option<SkillToolRuntimePlan>,
    ) -> Result<DispatchExecutionResult, OrchestratorCommandError> {
        self.execute_dispatch_flow(decision, worker_id, session_id, workspace_id, skill_plan)
    }

    pub fn execute_dispatch_then<F>(
        &self,
        decision: DispatchDecision,
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
            self.execute_dispatch_flow(decision, worker_id, session_id, workspace_id, skill_plan)?;
        on_success(&result);
        Ok(result)
    }

    pub fn execute_dispatch_with_writebacks(
        &self,
        decision: DispatchDecision,
        worker_id: WorkerId,
        session_id: Option<SessionId>,
        workspace_id: Option<WorkspaceId>,
        skill_plan: Option<SkillToolRuntimePlan>,
        memory_store: MemoryStore,
        writebacks: ExecutionWritebackPlans,
    ) -> Result<DispatchExecutionResult, OrchestratorCommandError> {
        self.execute_dispatch_then(
            decision,
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
        let mut decision = self
            .service
            .build_resume_dispatch_decision(&input)
            .ok_or(OrchestratorCommandError::NoResumeTarget {
                recovery_id: input.recovery_id.clone(),
            })?;
        // Recovery execution must report the same worker that actually runs the
        // resumed dispatch, even when the caller overrides the stored ownership.
        decision.worker_id = Some(worker_id.clone());

        let _ = self
            .worker_runtime
            .resume_from_dispatch_decision(&decision, worker_id.clone());

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
                .apply_resume_dispatch_decision(&session_id, &decision)
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
                mission_id: Some(decision.mission_id.clone()),
                task_id: Some(decision.task_id.clone()),
                worker_id: decision.worker_id.clone(),
                execution_chain_ref: decision.execution_chain_ref.clone(),
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

        self.service.resume_from_dispatch_decision(&decision).ok_or(
            OrchestratorCommandError::MissionNotFound {
                mission_id: decision.mission_id.clone(),
            },
        )?;
        let dispatch = self.execute_dispatch_flow(
            DispatchDecision {
                mission_id: decision.mission_id.clone(),
                assignment_id: decision.assignment_id.clone(),
                task_id: decision.task_id.clone(),
            },
            worker_id,
            input.ownership.session_id.clone(),
            input.ownership.workspace_id.clone(),
            skill_plan,
        )?;
        let mission_snapshot = dispatch.overview.mission.clone();
        Ok(RecoveryExecutionResult {
            recovery_input: input,
            resume_command,
            decision,
            dispatch,
            session_sidecar,
            workspace_recovery,
            mission_snapshot,
        })
    }

    fn build_context_summary_for_dispatch(
        &self,
        decision: &DispatchDecision,
        session_id: &Option<SessionId>,
        workspace_id: &Option<WorkspaceId>,
    ) -> Option<MissionContextSummary> {
        let context_runtime = self.context_runtime.as_ref()?;
        let context_config = self.context_config.as_ref()?;
        let session_id = session_id.clone()?;
        let workspace_id = workspace_id.clone()?;
        let descriptor = self.service.dispatch_context_descriptor(decision)?;

        let request = ExecutionContextAssemblyRequest {
            session_id,
            workspace_id,
            project_key: context_config.project_key.clone(),
            clues: ExecutionContextClues {
                mission: descriptor.mission_title,
                assignment: descriptor.assignment_title,
                todo: descriptor.task_title,
            },
            budget: context_config.budget.clone(),
        };

        Some(MissionContextSummary::from_context_assembly(
            &context_runtime.assemble_execution_context(&request),
        ))
    }

    fn execute_dispatch_flow(
        &self,
        decision: DispatchDecision,
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
                &decision,
                worker_id.clone(),
                session_id.clone(),
                workspace_id.clone(),
                skill_plan,
            )
            .ok_or(OrchestratorCommandError::TaskNotFound {
                task_id: decision.task_id.clone(),
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
            task_id: decision.task_id.clone(),
        });
        let outcome = loop_controller
            .step()
            .ok_or(OrchestratorCommandError::NoDispatchTarget {
                mission_id: decision.mission_id.clone(),
            })?;
        if let Some(report) = outcome.report.clone() {
            self.service
                .apply_worker_report(&report)
                .ok_or(OrchestratorCommandError::TaskNotFound {
                    task_id: report.task_id.clone(),
                })?;
        }
        let snapshot = self.worker_runtime.snapshot_for_task(&decision.task_id);
        for observation in &snapshot.skill_dispatches {
            let _ = self.service.apply_worker_skill_dispatch_observation(observation);
        }
        let tool_summary = match self.worker_runtime.executor_kind() {
            WorkerExecutorKind::LocalProcess => tool_summary_from_worker_snapshot(&snapshot),
            WorkerExecutorKind::Deterministic => {
                self.tool_registry.summary_for_query(&ToolExecutionContextQuery {
                    task_id: Some(decision.task_id.clone()),
                    ..ToolExecutionContextQuery::default()
                })
            }
        };
        let context_summary =
            self.build_context_summary_for_dispatch(&decision, &session_id, &workspace_id);
        let overview = self
            .service
            .build_execution_overview_with_context(
                &decision.mission_id,
                self.worker_runtime.summary(),
                tool_summary,
                &snapshot.skill_dispatches,
                &snapshot.governance_observations,
                context_summary,
            )
            .ok_or(OrchestratorCommandError::MissionNotFound {
                mission_id: decision.mission_id.clone(),
            })?;
        Ok(DispatchExecutionResult {
            decision,
            intent,
            outcome,
            overview,
        })
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
