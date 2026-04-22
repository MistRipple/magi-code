use crate::dispatch::{
    DispatchManager, DispatchResult, DispatchedTask,
};
use crate::{DispatchExecutionResult, OrchestratedExecutionRuntime, OrchestratorCommandError};
use magi_core::{AssignmentId, MissionId, TaskExecutionTarget, TaskId, TaskResultKind, WorkerId};
use magi_worker_runtime::WorkerLoopOutcomeKind;

#[derive(Clone, Debug)]
pub struct WorkerPipelineConfig {
    pub mission_id: MissionId,
    pub assignment_id: AssignmentId,
}

#[derive(Clone, Debug)]
pub struct WorkerPipelineResult {
    pub task_id: String,
    pub worker: String,
    pub success: bool,
    pub summary: String,
    pub modified_files: Vec<String>,
    pub errors: Vec<String>,
    pub execution_result: Option<DispatchExecutionResult>,
}

pub struct WorkerPipeline {
    config: WorkerPipelineConfig,
}

impl WorkerPipeline {
    pub fn new(config: WorkerPipelineConfig) -> Self {
        Self { config }
    }

    pub fn execute_task(
        &self,
        task: &DispatchedTask,
        runtime: &OrchestratedExecutionRuntime,
    ) -> WorkerPipelineResult {
        let target = TaskExecutionTarget {
            mission_id: self.config.mission_id.clone(),
            root_task_id: TaskId::new(task.task_id.clone()),
            task_id: TaskId::new(task.task_id.clone()),
            requested_worker_id: Some(WorkerId::new(task.worker.clone())),
            recovery_id: None,
            execution_chain_ref: None,
        };

        let worker_id = WorkerId::new(task.worker.clone());

        match runtime.execute_dispatch(target, worker_id, None, None, None) {
            Ok(exec_result) => self.build_success_result(task, exec_result),
            Err(err) => self.build_error_result(task, &err),
        }
    }

    pub fn execute_batch(
        &self,
        manager: &mut DispatchManager,
        runtime: &OrchestratedExecutionRuntime,
    ) -> Vec<WorkerPipelineResult> {
        let mut results = Vec::new();

        loop {
            let step = manager.step();
            if step.dispatched.is_empty() && step.skipped_idempotent.is_empty() {
                break;
            }

            for task in &step.dispatched {
                let result = self.execute_task(task, runtime);
                let dispatch_result = DispatchResult {
                    success: result.success,
                    summary: result.summary.clone(),
                    modified_files: if result.modified_files.is_empty() {
                        None
                    } else {
                        Some(result.modified_files.clone())
                    },
                    errors: if result.errors.is_empty() {
                        None
                    } else {
                        Some(result.errors.clone())
                    },
                    ..Default::default()
                };
                manager.complete_task(&task.task_id, dispatch_result);
                results.push(result);
            }

            if manager.is_all_completed() {
                break;
            }
        }

        results
    }

    fn build_success_result(
        &self,
        task: &DispatchedTask,
        exec_result: DispatchExecutionResult,
    ) -> WorkerPipelineResult {
        let outcome = &exec_result.outcome;
        let success = outcome.kind == WorkerLoopOutcomeKind::Applied;

        let summary = outcome
            .report
            .as_ref()
            .map(|r| r.summary.clone())
            .unwrap_or_else(|| format!("{:?}", outcome.kind));

        let is_task_success = outcome
            .report
            .as_ref()
            .and_then(|r| r.result_kind.as_ref())
            .map(|k| *k == TaskResultKind::Success)
            .unwrap_or(success);

        let modified_files = Vec::new();

        let errors = if !is_task_success && !summary.is_empty() {
            vec![summary.clone()]
        } else {
            Vec::new()
        };

        WorkerPipelineResult {
            task_id: task.task_id.clone(),
            worker: task.worker.clone(),
            success: is_task_success,
            summary,
            modified_files,
            errors,
            execution_result: Some(exec_result),
        }
    }

    fn build_error_result(
        &self,
        task: &DispatchedTask,
        err: &OrchestratorCommandError,
    ) -> WorkerPipelineResult {
        let error_msg = format!("{:?}", err);
        WorkerPipelineResult {
            task_id: task.task_id.clone(),
            worker: task.worker.clone(),
            success: false,
            summary: error_msg.clone(),
            modified_files: Vec::new(),
            errors: vec![error_msg],
            execution_result: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{DispatchManagerConfig, DispatchRoutingService};
    use magi_bridge_client::assignment_dispatch::{AssignmentDispatchPayload, AssignmentTask};
    use std::collections::HashMap;

    fn make_pipeline() -> WorkerPipeline {
        WorkerPipeline::new(WorkerPipelineConfig {
            mission_id: MissionId::new("mission-test".to_string()),
            assignment_id: AssignmentId::new("assignment-test".to_string()),
        })
    }

    fn make_manager() -> DispatchManager {
        let workers = vec!["backend".to_string(), "frontend".to_string()];
        let routing = DispatchRoutingService::new(workers, HashMap::new(), 60_000);
        DispatchManager::new(
            DispatchManagerConfig {
                session_id: "sess-1".to_string(),
                mission_id: "mission-test".to_string(),
                allow_busy_fallback: false,
            },
            routing,
        )
    }

    fn make_payload() -> AssignmentDispatchPayload {
        AssignmentDispatchPayload {
            mission_title: Some("测试任务".to_string()),
            tasks: vec![AssignmentTask {
                task_name: "实现用户登录".to_string(),
                ownership_hint: "backend".to_string(),
                mode_hint: "implement".to_string(),
                goal: "实现登录接口".to_string(),
                acceptance: vec![],
                constraints: vec![],
                context: vec![],
                requires_modification: true,
            }],
        }
    }

    #[test]
    fn pipeline_config_constructs() {
        let pipeline = make_pipeline();
        assert_eq!(pipeline.config.mission_id.to_string(), "mission-test");
    }

    #[test]
    fn pipeline_error_result_captures_details() {
        let pipeline = make_pipeline();
        let task = DispatchedTask {
            task_id: "task-1".to_string(),
            worker: "backend".to_string(),
            task_contract: Default::default(),
        };
        let err = OrchestratorCommandError::TaskNotFound {
            task_id: TaskId::new("task-1".to_string()),
        };
        let result = pipeline.build_error_result(&task, &err);
        assert!(!result.success);
        assert_eq!(result.errors.len(), 1);
        assert!(result.execution_result.is_none());
    }

    #[test]
    fn manager_step_provides_dispatched_tasks() {
        let mut manager = make_manager();
        manager.prepare_from_assignment(&make_payload(), None);
        let step = manager.step();
        assert_eq!(step.dispatched.len(), 1);
        assert_eq!(step.dispatched[0].worker, "backend");
    }
}
