use crate::{
    OrchestratorCommand, OrchestratorCommandError, OrchestratorCommandResult,
    OrchestratorControlPlane,
};

impl OrchestratorControlPlane {
    pub fn execute(
        &self,
        command: OrchestratorCommand,
    ) -> Result<OrchestratorCommandResult, OrchestratorCommandError> {
        match command {
            OrchestratorCommand::CreateMission { mission_id, title } => {
                let mission = self.service.create_mission(mission_id, title);
                Ok(OrchestratorCommandResult::MissionCreated { mission })
            }
            OrchestratorCommand::AddAssignment {
                mission_id,
                assignment_id,
                title,
            } => {
                if !self.service.mission_exists(&mission_id) {
                    return Err(OrchestratorCommandError::MissionNotFound { mission_id });
                }
                let mission = self
                    .service
                    .add_assignment(&mission_id, assignment_id.clone(), title)
                    .ok_or(OrchestratorCommandError::MissionNotFound {
                        mission_id: mission_id.clone(),
                    })?;
                Ok(OrchestratorCommandResult::AssignmentAdded { mission })
            }
            OrchestratorCommand::CreateTask {
                mission_id,
                assignment_id,
                task_id,
                title,
            } => {
                if !self.service.mission_exists(&mission_id) {
                    return Err(OrchestratorCommandError::MissionNotFound { mission_id });
                }
                if !self.service.assignment_exists(&mission_id, &assignment_id) {
                    return Err(OrchestratorCommandError::AssignmentNotFound {
                        mission_id,
                        assignment_id,
                    });
                }
                let mission = self
                    .service
                    .create_task(&mission_id, &assignment_id, task_id.clone(), title)
                    .ok_or(OrchestratorCommandError::AssignmentNotFound {
                        mission_id: mission_id.clone(),
                        assignment_id: assignment_id.clone(),
                    })?;
                Ok(OrchestratorCommandResult::TaskCreated { mission })
            }
            OrchestratorCommand::DispatchNextTask { mission_id } => {
                let decision = self
                    .service
                    .dispatch_next_task(&mission_id)
                    .ok_or(OrchestratorCommandError::NoDispatchTarget {
                        mission_id: mission_id.clone(),
                    })?;
                Ok(OrchestratorCommandResult::TaskDispatchPlanned { decision })
            }
            OrchestratorCommand::ApplyWorkerReport { report } => {
                let mission = self
                    .service
                    .apply_worker_report(&report)
                    .ok_or(OrchestratorCommandError::TaskNotFound {
                        task_id: report.task_id.clone(),
                    })?;
                Ok(OrchestratorCommandResult::WorkerReportApplied { mission })
            }
            OrchestratorCommand::ApplyWorkerSkillDispatchObservation { observation } => {
                let snapshot = self
                    .service
                    .apply_worker_skill_dispatch_observation(&observation)
                    .ok_or(OrchestratorCommandError::TaskNotFound {
                        task_id: observation.task_id.clone(),
                    })?;
                Ok(OrchestratorCommandResult::WorkerSkillDispatchObservationApplied {
                    snapshot,
                })
            }
            OrchestratorCommand::ApplyGovernanceDecision { request } => {
                let (mission, decision, disposition) =
                    self.service.apply_governance_decision(&request)?;
                Ok(OrchestratorCommandResult::GovernanceDecisionApplied {
                    mission,
                    decision,
                    disposition,
                })
            }
            OrchestratorCommand::BuildMissionExecutionOverview {
                mission_id,
                worker_summary,
                tool_summary,
                skill_dispatch_observations,
                governance_observations,
                context_summary,
            } => {
                let overview = self
                    .service
                    .build_execution_overview_with_context(
                        &mission_id,
                        worker_summary,
                        tool_summary,
                        &skill_dispatch_observations,
                        &governance_observations,
                        context_summary,
                    )
                    .ok_or(OrchestratorCommandError::MissionNotFound {
                        mission_id: mission_id.clone(),
                    })?;
                Ok(OrchestratorCommandResult::MissionExecutionOverviewBuilt {
                    overview,
                })
            }
            OrchestratorCommand::BuildResumeCommand { input } => {
                let command = self
                    .service
                    .build_resume_command(&input)
                    .ok_or(OrchestratorCommandError::NoResumeTarget {
                        recovery_id: input.recovery_id.clone(),
                    })?;
                Ok(OrchestratorCommandResult::ResumeCommandBuilt { command })
            }
        }
    }
}
