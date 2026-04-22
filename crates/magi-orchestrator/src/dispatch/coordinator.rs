use super::batch::{
    DispatchAuditOutcome, DispatchBatch, DispatchBatchEvent, DispatchStatus,
};

#[derive(Clone, Debug)]
pub enum CoordinatorAction {
    EmitWorkerInstruction {
        task_id: String,
        worker: String,
        batch_id: String,
    },
    ScheduleReadyTasks {
        batch_id: String,
        reason: String,
    },
    ClearProtocolState {
        task_id: String,
    },
    ClearProtocolStatesByBatch {
        batch_id: String,
    },
    UpdateDispatchStatus {
        task_id: String,
        status: DispatchStatus,
    },
    PushCompletionEntry {
        task_id: String,
    },
    ClearActiveWorkerLanes,
    ClearDispatchScheduleTimers {
        batch_id: Option<String>,
    },
    ClearResumeContext,
    ClearResumeDispatchGuard {
        batch_id: String,
    },
    TriggerPhaseCSummary {
        batch_id: String,
        audit_outcome: Option<DispatchAuditOutcome>,
    },
    NotifyBatchCancelled {
        batch_id: String,
        reason: String,
    },
    NotifyBatchInterrupted {
        batch_id: String,
        reason: String,
    },
}

pub struct DispatchBatchCoordinator;

impl DispatchBatchCoordinator {
    pub fn new() -> Self {
        Self
    }

    /// 处理 batch 上积累的事件，返回需要执行的动作列表。
    /// 调用方负责逐一执行这些动作。
    pub fn process_batch_events(
        &self,
        batch: &mut DispatchBatch,
    ) -> Vec<CoordinatorAction> {
        let events = batch.drain_events();
        if events.is_empty() {
            return Vec::new();
        }

        let batch_id = batch.id.clone();
        let mut actions = Vec::new();

        for event in events {
            match event {
                DispatchBatchEvent::TaskReady { task_id } => {
                    if let Some(entry) = batch.get_entry(&task_id) {
                        actions.push(CoordinatorAction::EmitWorkerInstruction {
                            task_id: task_id.clone(),
                            worker: entry.worker.clone(),
                            batch_id: batch_id.clone(),
                        });
                    }
                    actions.push(CoordinatorAction::ScheduleReadyTasks {
                        batch_id: batch_id.clone(),
                        reason: "task-ready".to_string(),
                    });
                }

                DispatchBatchEvent::TaskStatusChanged {
                    task_id,
                    status,
                    ..
                } => {
                    if let Some(entry) = batch.get_entry(&task_id) {
                        actions.push(CoordinatorAction::EmitWorkerInstruction {
                            task_id: task_id.clone(),
                            worker: entry.worker.clone(),
                            batch_id: batch_id.clone(),
                        });
                    }
                    if status.is_terminal() {
                        actions.push(CoordinatorAction::ClearProtocolState {
                            task_id: task_id.clone(),
                        });
                        let mapped = match status {
                            DispatchStatus::Completed => DispatchStatus::Completed,
                            DispatchStatus::Failed => DispatchStatus::Failed,
                            _ => DispatchStatus::Cancelled,
                        };
                        actions.push(CoordinatorAction::UpdateDispatchStatus {
                            task_id: task_id.clone(),
                            status: mapped,
                        });
                        actions.push(CoordinatorAction::ScheduleReadyTasks {
                            batch_id: batch_id.clone(),
                            reason: "task-terminal".to_string(),
                        });
                        actions.push(CoordinatorAction::PushCompletionEntry {
                            task_id,
                        });
                    }
                }

                DispatchBatchEvent::AllCompleted { batch_id: bid } => {
                    let audit = batch.audit_outcome().cloned();
                    actions.push(CoordinatorAction::TriggerPhaseCSummary {
                        batch_id: bid,
                        audit_outcome: audit,
                    });
                }

                DispatchBatchEvent::BatchCancelled { batch_id: bid, reason } => {
                    actions.extend(self.build_cleanup_actions(&bid));
                    actions.push(CoordinatorAction::NotifyBatchCancelled {
                        batch_id: bid,
                        reason,
                    });
                }

                DispatchBatchEvent::PhaseChanged { batch_id: bid, phase } => {
                    if phase == super::batch::BatchPhase::Archived {
                        actions.extend(self.build_cleanup_actions(&bid));
                        actions.push(CoordinatorAction::ClearResumeContext);
                    }
                }
            }
        }

        actions
    }

    fn build_cleanup_actions(&self, batch_id: &str) -> Vec<CoordinatorAction> {
        vec![
            CoordinatorAction::ClearProtocolStatesByBatch {
                batch_id: batch_id.to_string(),
            },
            CoordinatorAction::ClearActiveWorkerLanes,
            CoordinatorAction::ClearDispatchScheduleTimers {
                batch_id: Some(batch_id.to_string()),
            },
            CoordinatorAction::ClearResumeDispatchGuard {
                batch_id: batch_id.to_string(),
            },
        ]
    }
}

impl Default for DispatchBatchCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::batch::{DispatchResult, DispatchTaskContract};

    fn make_batch() -> DispatchBatch {
        let mut batch = DispatchBatch::new(Some("test-batch"));
        batch
            .register(
                "task-1",
                "worker-a",
                DispatchTaskContract {
                    task_title: "任务1".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
        batch
            .register(
                "task-2",
                "worker-b",
                DispatchTaskContract {
                    task_title: "任务2".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
        // drain 注册时产生的事件
        batch.drain_events();
        batch
    }

    #[test]
    fn task_completion_generates_actions() {
        let coordinator = DispatchBatchCoordinator::new();
        let mut batch = make_batch();
        batch.mark_running("task-1");
        batch.drain_events();
        batch.mark_completed(
            "task-1",
            DispatchResult {
                success: true,
                summary: "ok".to_string(),
                ..Default::default()
            },
        );
        let actions = coordinator.process_batch_events(&mut batch);
        let has_clear_protocol = actions.iter().any(|a| matches!(a, CoordinatorAction::ClearProtocolState { task_id } if task_id == "task-1"));
        let has_push_completion = actions.iter().any(|a| matches!(a, CoordinatorAction::PushCompletionEntry { task_id } if task_id == "task-1"));
        assert!(has_clear_protocol);
        assert!(has_push_completion);
    }

    #[test]
    fn cancel_all_generates_cleanup() {
        let coordinator = DispatchBatchCoordinator::new();
        let mut batch = make_batch();
        batch.cancel_all("用户取消");
        let actions = coordinator.process_batch_events(&mut batch);
        let has_clear_lanes = actions.iter().any(|a| matches!(a, CoordinatorAction::ClearActiveWorkerLanes));
        let has_cancel_notify = actions.iter().any(|a| matches!(a, CoordinatorAction::NotifyBatchCancelled { .. }));
        assert!(has_clear_lanes);
        assert!(has_cancel_notify);
    }

    #[test]
    fn all_completed_triggers_phase_c() {
        let coordinator = DispatchBatchCoordinator::new();
        let mut batch = make_batch();
        batch.mark_running("task-1");
        batch.mark_running("task-2");
        batch.drain_events();
        batch.mark_completed(
            "task-1",
            DispatchResult { success: true, summary: "ok".to_string(), ..Default::default() },
        );
        batch.mark_completed(
            "task-2",
            DispatchResult { success: true, summary: "ok".to_string(), ..Default::default() },
        );
        let actions = coordinator.process_batch_events(&mut batch);
        let has_phase_c = actions.iter().any(|a| matches!(a, CoordinatorAction::TriggerPhaseCSummary { .. }));
        assert!(has_phase_c);
    }

    #[test]
    fn empty_events_returns_empty() {
        let coordinator = DispatchBatchCoordinator::new();
        let mut batch = make_batch();
        let actions = coordinator.process_batch_events(&mut batch);
        assert!(actions.is_empty());
    }
}
