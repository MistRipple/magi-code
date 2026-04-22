use crate::dispatch::{
    DispatchManager, DispatchManagerConfig, DispatchRoutingService,
};
use crate::message::MessageHub;
use crate::worker_pipeline::{WorkerPipeline, WorkerPipelineConfig};
use crate::{OrchestratedExecutionRuntime, OrchestratorControlPlane, OrchestratorCommand};
use magi_bridge_client::assignment_dispatch::AssignmentDispatchPayload;
use magi_core::{AssignmentId, MissionId, TaskId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct OrchestrationLoopConfig {
    pub session_id: String,
    pub allow_busy_fallback: bool,
    pub max_dispatch_waves: u32,
}

impl Default for OrchestrationLoopConfig {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            allow_busy_fallback: false,
            max_dispatch_waves: 10,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationOutcome {
    Completed,
    PartiallyCompleted,
    Failed,
    NoTasks,
    WaveLimitReached,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestrationWaveSummary {
    pub wave: u32,
    pub batch_id: String,
    pub tasks_dispatched: usize,
    pub tasks_completed: usize,
    pub tasks_failed: usize,
    pub routing_failures: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestrationLoopResult {
    pub mission_id: String,
    pub outcome: OrchestrationOutcome,
    pub total_waves: u32,
    pub total_tasks: usize,
    pub completed_tasks: usize,
    pub failed_tasks: usize,
    pub wave_summaries: Vec<OrchestrationWaveSummary>,
}

pub struct OrchestrationLoopController {
    config: OrchestrationLoopConfig,
}

impl OrchestrationLoopController {
    pub fn new(config: OrchestrationLoopConfig) -> Self {
        Self { config }
    }

    pub fn execute_assignment_dispatch(
        &self,
        payload: &AssignmentDispatchPayload,
        mission_id: &MissionId,
        assignment_id: &AssignmentId,
        control_plane: &OrchestratorControlPlane,
        runtime: &OrchestratedExecutionRuntime,
        routing: DispatchRoutingService,
        message_hub: &mut MessageHub,
    ) -> OrchestrationLoopResult {
        let title = payload
            .mission_title
            .as_deref()
            .unwrap_or("未命名任务");

        message_hub.emit_mission_created(mission_id.clone(), title);

        self.ensure_mission_and_assignment(
            control_plane,
            mission_id,
            assignment_id,
            title,
            &payload.tasks,
        );

        let mut manager = DispatchManager::new(
            DispatchManagerConfig {
                session_id: self.config.session_id.clone(),
                mission_id: mission_id.to_string(),
                allow_busy_fallback: self.config.allow_busy_fallback,
            },
            routing,
        );

        let pipeline = WorkerPipeline::new(WorkerPipelineConfig {
            mission_id: mission_id.clone(),
            assignment_id: assignment_id.clone(),
        });

        let prepare = manager.prepare_from_assignment(payload, None);

        if prepare.entries.is_empty() {
            for failure in &prepare.routing_failures {
                message_hub.emit_error(
                    crate::message::MessageContext::for_mission(mission_id.clone()),
                    &failure.error,
                    "routing",
                );
            }
            return OrchestrationLoopResult {
                mission_id: mission_id.to_string(),
                outcome: OrchestrationOutcome::NoTasks,
                total_waves: 0,
                total_tasks: 0,
                completed_tasks: 0,
                failed_tasks: 0,
                wave_summaries: Vec::new(),
            };
        }

        message_hub.emit_dispatch_started(
            mission_id.clone(),
            &prepare.batch_id,
            prepare.entries.len(),
        );

        let mut wave_summaries = Vec::new();
        let mut total_completed = 0usize;
        let mut total_failed = 0usize;
        let mut wave = 0u32;

        loop {
            if wave >= self.config.max_dispatch_waves {
                break;
            }
            wave += 1;

            let results = pipeline.execute_batch(&mut manager, runtime);
            if results.is_empty() {
                break;
            }

            let mut wave_completed = 0usize;
            let mut wave_failed = 0usize;

            for result in &results {
                message_hub.emit_worker_lifecycle(result, mission_id.clone());
                if result.success {
                    wave_completed += 1;
                } else {
                    wave_failed += 1;
                }
            }

            total_completed += wave_completed;
            total_failed += wave_failed;

            wave_summaries.push(OrchestrationWaveSummary {
                wave,
                batch_id: prepare.batch_id.clone(),
                tasks_dispatched: results.len(),
                tasks_completed: wave_completed,
                tasks_failed: wave_failed,
                routing_failures: prepare.routing_failures.len(),
            });

            if manager.is_all_completed() {
                break;
            }
        }

        let summary = manager.summary();
        message_hub.emit_dispatch_completed(
            mission_id.clone(),
            &prepare.batch_id,
            &summary,
        );

        let total_tasks = summary.total;
        let outcome = if total_failed == 0 && total_completed == total_tasks {
            message_hub.emit_mission_completed(mission_id.clone(), "所有任务已完成");
            OrchestrationOutcome::Completed
        } else if total_completed > 0 && total_failed > 0 {
            message_hub.emit_mission_completed(
                mission_id.clone(),
                &format!("{} 完成 / {} 失败", total_completed, total_failed),
            );
            OrchestrationOutcome::PartiallyCompleted
        } else if wave >= self.config.max_dispatch_waves && !manager.is_all_completed() {
            OrchestrationOutcome::WaveLimitReached
        } else {
            message_hub.emit_mission_failed(mission_id.clone(), "所有任务失败");
            OrchestrationOutcome::Failed
        };

        OrchestrationLoopResult {
            mission_id: mission_id.to_string(),
            outcome,
            total_waves: wave,
            total_tasks,
            completed_tasks: total_completed,
            failed_tasks: total_failed,
            wave_summaries,
        }
    }

    fn ensure_mission_and_assignment(
        &self,
        control_plane: &OrchestratorControlPlane,
        mission_id: &MissionId,
        assignment_id: &AssignmentId,
        title: &str,
        tasks: &[magi_bridge_client::assignment_dispatch::AssignmentTask],
    ) {
        let _ = control_plane.execute(OrchestratorCommand::CreateMission {
            mission_id: mission_id.clone(),
            title: title.to_string(),
        });

        let _ = control_plane.execute(OrchestratorCommand::AddAssignment {
            mission_id: mission_id.clone(),
            assignment_id: assignment_id.clone(),
            title: title.to_string(),
        });

        for (i, task) in tasks.iter().enumerate() {
            let task_id = TaskId::new(format!("dispatch_{}_{}", sanitize_name(&task.task_name), i));
            let _ = control_plane.execute(OrchestratorCommand::CreateTask {
                mission_id: mission_id.clone(),
                assignment_id: assignment_id.clone(),
                task_id,
                title: task.task_name.clone(),
            });
        }
    }
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_config_defaults() {
        let config = OrchestrationLoopConfig::default();
        assert_eq!(config.max_dispatch_waves, 10);
        assert!(!config.allow_busy_fallback);
    }

    #[test]
    fn sanitize_name_handles_chinese() {
        let result = sanitize_name("实现用户登录");
        assert!(!result.is_empty());
        assert!(!result.contains(' '));
    }

    #[test]
    fn orchestration_outcome_serializes() {
        let outcome = OrchestrationOutcome::Completed;
        let json = serde_json::to_string(&outcome).unwrap();
        assert_eq!(json, "\"completed\"");
    }

    #[test]
    fn loop_result_serializes() {
        let result = OrchestrationLoopResult {
            mission_id: "m-1".to_string(),
            outcome: OrchestrationOutcome::PartiallyCompleted,
            total_waves: 2,
            total_tasks: 5,
            completed_tasks: 3,
            failed_tasks: 2,
            wave_summaries: vec![],
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("partially_completed"));
        assert!(json.contains("\"total_tasks\":5"));
    }
}
