use std::collections::HashSet;

use magi_bridge_client::assignment_dispatch::{AssignmentDispatchPayload, AssignmentTask};

use super::batch::{
    DispatchBatch, DispatchBatchSummary, DispatchCollaborationContracts, DispatchResult,
    DispatchStatus, DispatchTaskContract,
};
use super::completion::{DispatchCompletionQueue, WorkerCompletionResult};
use super::idempotency::{
    DispatchIdempotencyClaimInput, DispatchIdempotencyStatus, DispatchIdempotencyStore,
};
use super::routing::DispatchRoutingService;

#[derive(Clone, Debug)]
pub struct DispatchManagerConfig {
    pub session_id: String,
    pub mission_id: String,
    pub allow_busy_fallback: bool,
}

#[derive(Clone, Debug)]
pub struct PreparedEntry {
    pub task_id: String,
    pub worker: String,
    pub routing_degraded: bool,
    pub routing_reason: String,
}

#[derive(Clone, Debug)]
pub struct PrepareResult {
    pub batch_id: String,
    pub entries: Vec<PreparedEntry>,
    pub routing_failures: Vec<RoutingFailure>,
}

#[derive(Clone, Debug)]
pub struct RoutingFailure {
    pub task_name: String,
    pub ownership_hint: String,
    pub error: String,
}

#[derive(Clone, Debug)]
pub struct BatchStepResult {
    pub dispatched: Vec<DispatchedTask>,
    pub skipped_idempotent: Vec<String>,
    pub all_completed: bool,
}

#[derive(Clone, Debug)]
pub struct DispatchedTask {
    pub task_id: String,
    pub worker: String,
    pub task_contract: DispatchTaskContract,
}

pub struct DispatchManager {
    config: DispatchManagerConfig,
    batch: DispatchBatch,
    routing: DispatchRoutingService,
    idempotency: DispatchIdempotencyStore,
    completion_queue: DispatchCompletionQueue,
}

impl DispatchManager {
    pub fn new(
        config: DispatchManagerConfig,
        routing: DispatchRoutingService,
    ) -> Self {
        Self {
            config,
            batch: DispatchBatch::new(None),
            routing,
            idempotency: DispatchIdempotencyStore::default(),
            completion_queue: DispatchCompletionQueue::default(),
        }
    }

    pub fn prepare_from_assignment(
        &mut self,
        payload: &AssignmentDispatchPayload,
        dispatch_id: Option<&str>,
    ) -> PrepareResult {
        self.batch = DispatchBatch::new(dispatch_id);
        if let Some(title) = &payload.mission_title {
            self.batch.set_user_prompt(title);
        }

        let mut entries = Vec::new();
        let mut routing_failures = Vec::new();

        for task in &payload.tasks {
            let task_id = build_task_id(&task.task_name, entries.len());
            let contract = build_task_contract(task);

            let resolution = self.routing.resolve_execution_worker(
                &task.ownership_hint,
                None,
                None,
                self.config.allow_busy_fallback,
            );

            if !resolution.ok {
                routing_failures.push(RoutingFailure {
                    task_name: task.task_name.clone(),
                    ownership_hint: task.ownership_hint.clone(),
                    error: resolution.error.unwrap_or_default(),
                });
                continue;
            }

            let worker = resolution.selected_worker.unwrap_or_default();

            match self.batch.register(&task_id, &worker, contract) {
                Ok(_) => {
                    entries.push(PreparedEntry {
                        task_id,
                        worker,
                        routing_degraded: resolution.degraded,
                        routing_reason: resolution.routing_reason,
                    });
                }
                Err(e) => {
                    routing_failures.push(RoutingFailure {
                        task_name: task.task_name.clone(),
                        ownership_hint: task.ownership_hint.clone(),
                        error: e,
                    });
                }
            }
        }

        PrepareResult {
            batch_id: self.batch.id.clone(),
            entries,
            routing_failures,
        }
    }

    pub fn step(&mut self) -> BatchStepResult {
        let ready_snapshot: Vec<(String, String, DispatchTaskContract)> = self
            .batch
            .get_ready_tasks_isolated()
            .iter()
            .map(|e| (e.task_id.clone(), e.worker.clone(), e.task_contract.clone()))
            .collect();

        let mut dispatched = Vec::new();
        let mut skipped_idempotent = Vec::new();

        for (task_id, worker, task_contract) in &ready_snapshot {
            let idem_key = build_idempotency_key(
                &self.config.session_id,
                &self.config.mission_id,
                &task_contract.task_title,
                &task_contract.ownership,
            );

            let claim = self.idempotency.claim_or_get(DispatchIdempotencyClaimInput {
                key: idem_key,
                session_id: self.config.session_id.clone(),
                mission_id: self.config.mission_id.clone(),
                task_id: task_id.clone(),
                worker: worker.clone(),
                ownership: task_contract.ownership.clone(),
                mode: task_contract.mode.clone(),
                task_name: task_contract.task_title.clone(),
                routing_reason: String::new(),
                degraded: false,
                status: DispatchIdempotencyStatus::Dispatched,
                created_at: None,
                updated_at: None,
            });

            if !claim.claimed {
                skipped_idempotent.push(task_id.clone());
                self.batch.update_status(
                    task_id,
                    DispatchStatus::Skipped,
                    Some(DispatchResult {
                        success: false,
                        summary: format!("幂等检查：任务已被派发 (key={})", claim.record.key),
                        ..Default::default()
                    }),
                );
                continue;
            }

            self.batch.mark_running(task_id);

            dispatched.push(DispatchedTask {
                task_id: task_id.clone(),
                worker: worker.clone(),
                task_contract: task_contract.clone(),
            });
        }

        BatchStepResult {
            dispatched,
            skipped_idempotent,
            all_completed: self.batch.is_all_completed(),
        }
    }

    pub fn complete_task(&mut self, task_id: &str, result: DispatchResult) {
        let status = if result.success {
            DispatchStatus::Completed
        } else {
            DispatchStatus::Failed
        };
        self.batch.update_status(task_id, status, Some(result));

        let idem_status = if status == DispatchStatus::Completed {
            DispatchIdempotencyStatus::Completed
        } else {
            DispatchIdempotencyStatus::Failed
        };
        self.idempotency
            .update_status_by_task_id(task_id, idem_status);

        self.completion_queue.push_from_batch(&self.batch, task_id);
    }

    pub fn cancel_task(&mut self, task_id: &str, reason: &str) {
        self.batch.update_status(
            task_id,
            DispatchStatus::Cancelled,
            Some(DispatchResult {
                success: false,
                summary: reason.to_string(),
                ..Default::default()
            }),
        );
        self.idempotency
            .update_status_by_task_id(task_id, DispatchIdempotencyStatus::Cancelled);
        self.completion_queue.push_from_batch(&self.batch, task_id);
    }

    pub fn cancel_all(&mut self, reason: &str) {
        self.batch
            .cancellation_token_mut()
            .cancel(reason);

        let pending_ids: Vec<String> = self
            .batch
            .entries()
            .iter()
            .filter(|e| !e.status.is_terminal())
            .map(|e| e.task_id.clone())
            .collect();

        for task_id in &pending_ids {
            self.cancel_task(task_id, reason);
        }
    }

    pub fn drain_completions(&mut self) -> Vec<WorkerCompletionResult> {
        self.completion_queue.drain_all()
    }

    pub fn drain_completions_for(
        &mut self,
        target_ids: &HashSet<String>,
    ) -> Vec<WorkerCompletionResult> {
        self.completion_queue
            .drain_for_targets(target_ids, Some(&self.batch))
    }

    pub fn is_all_completed(&self) -> bool {
        self.batch.is_all_completed()
    }

    pub fn summary(&self) -> DispatchBatchSummary {
        self.batch.summary()
    }

    pub fn batch(&self) -> &DispatchBatch {
        &self.batch
    }

    pub fn batch_mut(&mut self) -> &mut DispatchBatch {
        &mut self.batch
    }

    pub fn routing_mut(&mut self) -> &mut DispatchRoutingService {
        &mut self.routing
    }
}

fn build_task_id(task_name: &str, index: usize) -> String {
    let sanitized: String = task_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let truncated = if sanitized.len() > 40 {
        &sanitized[..40]
    } else {
        &sanitized
    };
    format!("dispatch_{}_{}", truncated, index)
}

fn build_task_contract(task: &AssignmentTask) -> DispatchTaskContract {
    DispatchTaskContract {
        task_title: task.task_name.clone(),
        ownership: task.ownership_hint.clone(),
        mode: task.mode_hint.clone(),
        context: task.context.clone(),
        scope_hint: task.acceptance.clone(),
        files: Vec::new(),
        depends_on: Vec::new(),
        collaboration_contracts: DispatchCollaborationContracts::default(),
    }
}

fn build_idempotency_key(
    session_id: &str,
    mission_id: &str,
    task_name: &str,
    ownership: &str,
) -> String {
    format!(
        "{}:{}:{}:{}",
        session_id, mission_id, task_name, ownership
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_routing() -> DispatchRoutingService {
        let workers = vec!["backend".to_string(), "frontend".to_string()];
        let fallback: HashMap<String, Vec<String>> = HashMap::new();
        DispatchRoutingService::new(workers, fallback, 60_000)
    }

    fn make_config() -> DispatchManagerConfig {
        DispatchManagerConfig {
            session_id: "sess-1".to_string(),
            mission_id: "mission-1".to_string(),
            allow_busy_fallback: false,
        }
    }

    fn make_payload() -> AssignmentDispatchPayload {
        AssignmentDispatchPayload {
            mission_title: Some("测试任务".to_string()),
            tasks: vec![
                AssignmentTask {
                    task_name: "实现用户登录".to_string(),
                    ownership_hint: "backend".to_string(),
                    mode_hint: "implement".to_string(),
                    goal: "实现登录接口".to_string(),
                    acceptance: vec!["通过测试".to_string()],
                    constraints: vec![],
                    context: vec!["auth 模块".to_string()],
                    requires_modification: true,
                },
                AssignmentTask {
                    task_name: "实现登录页面".to_string(),
                    ownership_hint: "frontend".to_string(),
                    mode_hint: "implement".to_string(),
                    goal: "实现登录 UI".to_string(),
                    acceptance: vec![],
                    constraints: vec![],
                    context: vec![],
                    requires_modification: true,
                },
            ],
        }
    }

    #[test]
    fn prepare_registers_all_tasks() {
        let mut mgr = DispatchManager::new(make_config(), make_routing());
        let result = mgr.prepare_from_assignment(&make_payload(), Some("batch-test"));
        assert_eq!(result.entries.len(), 2);
        assert!(result.routing_failures.is_empty());
        assert_eq!(result.batch_id, "batch-test");
    }

    #[test]
    fn prepare_reports_routing_failure() {
        let mut mgr = DispatchManager::new(make_config(), make_routing());
        let payload = AssignmentDispatchPayload {
            mission_title: None,
            tasks: vec![AssignmentTask {
                task_name: "未知任务".to_string(),
                ownership_hint: "nonexistent_worker".to_string(),
                mode_hint: "implement".to_string(),
                goal: "test".to_string(),
                acceptance: vec![],
                constraints: vec![],
                context: vec![],
                requires_modification: false,
            }],
        };
        let result = mgr.prepare_from_assignment(&payload, None);
        assert!(result.entries.is_empty());
        assert_eq!(result.routing_failures.len(), 1);
    }

    #[test]
    fn step_dispatches_ready_tasks() {
        let mut mgr = DispatchManager::new(make_config(), make_routing());
        mgr.prepare_from_assignment(&make_payload(), None);
        let step = mgr.step();
        assert_eq!(step.dispatched.len(), 2);
        assert!(step.skipped_idempotent.is_empty());
        assert!(!step.all_completed);
    }

    #[test]
    fn step_skips_idempotent_duplicates() {
        let mut mgr = DispatchManager::new(make_config(), make_routing());
        mgr.prepare_from_assignment(&make_payload(), None);

        let step1 = mgr.step();
        assert_eq!(step1.dispatched.len(), 2);

        for d in &step1.dispatched {
            mgr.complete_task(
                &d.task_id,
                DispatchResult {
                    success: true,
                    summary: "ok".to_string(),
                    ..Default::default()
                },
            );
        }

        let payload2 = make_payload();
        mgr.prepare_from_assignment(&payload2, Some("batch-2"));
        let step2 = mgr.step();
        assert_eq!(step2.skipped_idempotent.len(), 2);
        assert!(step2.dispatched.is_empty());
    }

    #[test]
    fn complete_task_updates_state() {
        let mut mgr = DispatchManager::new(make_config(), make_routing());
        mgr.prepare_from_assignment(&make_payload(), None);
        let step = mgr.step();

        let task_id = &step.dispatched[0].task_id;
        mgr.complete_task(
            task_id,
            DispatchResult {
                success: true,
                summary: "任务完成".to_string(),
                ..Default::default()
            },
        );

        let entry = mgr.batch().get_entry(task_id).unwrap();
        assert_eq!(entry.status, DispatchStatus::Completed);

        let completions = mgr.drain_completions();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].task_id, *task_id);
    }

    #[test]
    fn cancel_all_cancels_pending_tasks() {
        let mut mgr = DispatchManager::new(make_config(), make_routing());
        mgr.prepare_from_assignment(&make_payload(), None);

        mgr.cancel_all("用户取消");

        let summary = mgr.summary();
        assert_eq!(summary.cancelled, 2);
        assert!(mgr.is_all_completed());
    }

    #[test]
    fn full_lifecycle() {
        let mut mgr = DispatchManager::new(make_config(), make_routing());
        mgr.prepare_from_assignment(&make_payload(), None);

        let step = mgr.step();
        assert_eq!(step.dispatched.len(), 2);

        for d in &step.dispatched {
            mgr.complete_task(
                &d.task_id,
                DispatchResult {
                    success: true,
                    summary: format!("{} 完成", d.task_id),
                    modified_files: Some(vec!["src/main.rs".to_string()]),
                    ..Default::default()
                },
            );
        }

        assert!(mgr.is_all_completed());
        let summary = mgr.summary();
        assert_eq!(summary.completed, 2);
        assert_eq!(summary.total, 2);

        let completions = mgr.drain_completions();
        assert_eq!(completions.len(), 2);
    }
}
