use std::collections::HashSet;

use super::batch::{DispatchBatch, DispatchEntry};
use super::routing::DispatchExecutionWorkerResolution;

pub struct DispatchSchedulerConfig {
    pub worker_lane_resident_timeout_ms: u64,
}

pub struct ScheduledWorkerLaunch {
    pub worker: String,
    pub entries: Vec<ScheduledEntry>,
}

pub struct ScheduledEntry {
    pub task_id: String,
    pub worker: String,
    pub routing_adjusted: bool,
    pub original_worker: Option<String>,
    pub routing_reason: String,
}

pub struct DispatchScheduler {
    config: DispatchSchedulerConfig,
    active_worker_lanes: HashSet<String>,
}

impl DispatchScheduler {
    pub fn new(config: DispatchSchedulerConfig) -> Self {
        Self {
            config,
            active_worker_lanes: HashSet::new(),
        }
    }

    /// 调度就绪任务，返回需要启动的 worker lane 列表。
    /// 实现同类串行/异类并行的隔离策略。
    pub fn schedule_ready_tasks<F>(
        &mut self,
        batch: &mut DispatchBatch,
        resolve_worker: F,
    ) -> Vec<ScheduledWorkerLaunch>
    where
        F: Fn(&str, Option<&HashSet<String>>, bool) -> DispatchExecutionWorkerResolution,
    {
        if batch.phase() != super::batch::BatchPhase::Active {
            return Vec::new();
        }

        let ready_tasks: Vec<(String, String)> = batch
            .get_ready_tasks()
            .iter()
            .map(|e| (e.task_id.clone(), e.worker.clone()))
            .collect();

        if ready_tasks.is_empty() {
            return Vec::new();
        }

        let busy_workers: HashSet<String> = self.active_worker_lanes.clone();
        let mut selected: Vec<ScheduledEntry> = Vec::new();
        let mut selected_workers: HashSet<String> = HashSet::new();

        for (task_id, preferred_worker) in &ready_tasks {
            let resolution = resolve_worker(
                preferred_worker,
                Some(&busy_workers),
                false,
            );

            if !resolution.ok {
                tracing::debug!(
                    task_id = %task_id,
                    worker = %preferred_worker,
                    error = ?resolution.error,
                    "就绪任务暂不可执行"
                );
                continue;
            }

            let selected_worker = match &resolution.selected_worker {
                Some(w) => w.clone(),
                None => continue,
            };

            if busy_workers.contains(&selected_worker) || selected_workers.contains(&selected_worker) {
                continue;
            }

            let routing_adjusted = selected_worker != *preferred_worker;
            selected_workers.insert(selected_worker.clone());

            selected.push(ScheduledEntry {
                task_id: task_id.clone(),
                worker: selected_worker.clone(),
                routing_adjusted,
                original_worker: if routing_adjusted {
                    Some(preferred_worker.clone())
                } else {
                    None
                },
                routing_reason: resolution.routing_reason,
            });
        }

        let mut launches: Vec<ScheduledWorkerLaunch> = Vec::new();
        for entry in selected {
            let worker = entry.worker.clone();
            if let Some(launch) = launches.iter_mut().find(|l| l.worker == worker) {
                launch.entries.push(entry);
            } else {
                launches.push(ScheduledWorkerLaunch {
                    worker,
                    entries: vec![entry],
                });
            }
        }

        launches
    }

    pub fn activate_worker_lane(&mut self, worker: &str) -> bool {
        self.active_worker_lanes.insert(worker.to_string())
    }

    pub fn release_worker_lane(&mut self, worker: &str) {
        self.active_worker_lanes.remove(worker);
    }

    pub fn is_worker_lane_active(&self, worker: &str) -> bool {
        self.active_worker_lanes.contains(worker)
    }

    pub fn active_worker_lanes(&self) -> &HashSet<String> {
        &self.active_worker_lanes
    }

    pub fn clear_active_worker_lanes(&mut self) {
        self.active_worker_lanes.clear();
    }

    pub fn get_next_ready_task_for_worker<'a>(
        &self,
        batch: &'a DispatchBatch,
        worker: &str,
    ) -> Option<&'a DispatchEntry> {
        batch
            .get_ready_tasks()
            .into_iter()
            .find(|e| e.worker == worker)
    }

    pub fn worker_lane_resident_timeout_ms(&self) -> u64 {
        self.config.worker_lane_resident_timeout_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::batch::DispatchTaskContract;

    fn make_config() -> DispatchSchedulerConfig {
        DispatchSchedulerConfig {
            worker_lane_resident_timeout_ms: 30_000,
        }
    }

    fn make_batch() -> DispatchBatch {
        let mut batch = DispatchBatch::new(Some("batch-1"));
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
        batch
            .register(
                "task-3",
                "worker-a",
                DispatchTaskContract {
                    task_title: "任务3".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
        batch
    }

    fn simple_resolve(
        preferred: &str,
        _busy: Option<&HashSet<String>>,
        _allow_busy_fallback: bool,
    ) -> DispatchExecutionWorkerResolution {
        DispatchExecutionWorkerResolution {
            ok: true,
            selected_worker: Some(preferred.to_string()),
            degraded: false,
            routing_reason: "direct".to_string(),
            error: None,
        }
    }

    #[test]
    fn schedules_one_per_worker() {
        let mut scheduler = DispatchScheduler::new(make_config());
        let mut batch = make_batch();
        let launches = scheduler.schedule_ready_tasks(&mut batch, simple_resolve);
        // worker-a 和 worker-b 各一个
        assert_eq!(launches.len(), 2);
        let workers: HashSet<String> = launches.iter().map(|l| l.worker.clone()).collect();
        assert!(workers.contains("worker-a"));
        assert!(workers.contains("worker-b"));
    }

    #[test]
    fn skips_busy_workers() {
        let mut scheduler = DispatchScheduler::new(make_config());
        scheduler.activate_worker_lane("worker-a");
        let mut batch = make_batch();
        let launches = scheduler.schedule_ready_tasks(&mut batch, simple_resolve);
        // worker-a 被占用，只有 worker-b
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].worker, "worker-b");
    }

    #[test]
    fn worker_lane_lifecycle() {
        let mut scheduler = DispatchScheduler::new(make_config());
        assert!(!scheduler.is_worker_lane_active("worker-a"));
        scheduler.activate_worker_lane("worker-a");
        assert!(scheduler.is_worker_lane_active("worker-a"));
        scheduler.release_worker_lane("worker-a");
        assert!(!scheduler.is_worker_lane_active("worker-a"));
    }

    #[test]
    fn clear_all_lanes() {
        let mut scheduler = DispatchScheduler::new(make_config());
        scheduler.activate_worker_lane("worker-a");
        scheduler.activate_worker_lane("worker-b");
        scheduler.clear_active_worker_lanes();
        assert!(scheduler.active_worker_lanes().is_empty());
    }
}
