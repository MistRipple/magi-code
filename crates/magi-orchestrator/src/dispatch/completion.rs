use super::batch::DispatchBatch;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerCompletionResult {
    pub task_id: String,
    pub worker: String,
    pub status: String,
    pub summary: String,
    #[serde(default)]
    pub modified_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitForWorkersResult {
    pub results: Vec<WorkerCompletionResult>,
    pub wait_status: String,
    pub timed_out: bool,
    pub pending_task_ids: Vec<String>,
    pub waited_ms: u64,
}

pub struct DispatchCompletionQueue {
    queue: Vec<WorkerCompletionResult>,
    pushed_task_ids: HashSet<String>,
}

impl DispatchCompletionQueue {
    pub fn new() -> Self {
        Self {
            queue: Vec::new(),
            pushed_task_ids: HashSet::new(),
        }
    }

    pub fn reset(&mut self) {
        self.queue.clear();
        self.pushed_task_ids.clear();
    }

    pub fn push(&mut self, task_id: &str, worker: &str, status: &str, summary: &str, modified_files: Vec<String>, errors: Option<Vec<String>>) {
        if self.pushed_task_ids.contains(task_id) {
            return;
        }
        self.pushed_task_ids.insert(task_id.to_string());

        self.queue.push(WorkerCompletionResult {
            task_id: task_id.to_string(),
            worker: worker.to_string(),
            status: status.to_string(),
            summary: summary.to_string(),
            modified_files,
            errors,
        });
    }

    pub fn push_from_batch(&mut self, batch: &DispatchBatch, task_id: &str) {
        if self.pushed_task_ids.contains(task_id) {
            return;
        }
        let Some(entry) = batch.get_entry(task_id) else {
            return;
        };
        if !entry.status.is_terminal() {
            return;
        }

        let status = format!("{:?}", entry.status).to_lowercase();
        let summary = entry
            .result
            .as_ref()
            .map(|r| r.summary.clone())
            .unwrap_or_default();
        let modified_files = entry
            .result
            .as_ref()
            .and_then(|r| r.modified_files.clone())
            .unwrap_or_default();
        let errors = entry.result.as_ref().and_then(|r| r.errors.clone());

        self.push(task_id, &entry.worker, &status, &summary, modified_files, errors);
    }

    pub fn drain_all(&mut self) -> Vec<WorkerCompletionResult> {
        std::mem::take(&mut self.queue)
    }

    pub fn drain_for_targets(
        &mut self,
        target_ids: &HashSet<String>,
        batch: Option<&DispatchBatch>,
    ) -> Vec<WorkerCompletionResult> {
        let mut matched = Vec::new();
        let mut remaining = Vec::new();

        for result in self.queue.drain(..) {
            if target_ids.contains(&result.task_id) {
                matched.push(result);
            } else {
                remaining.push(result);
            }
        }

        if let Some(batch) = batch {
            let matched_ids: HashSet<String> = matched.iter().map(|r| r.task_id.clone()).collect();
            for task_id in target_ids {
                if matched_ids.contains(task_id) {
                    continue;
                }
                if let Some(entry) = batch.get_entry(task_id) {
                    if entry.status.is_terminal() {
                        let status = format!("{:?}", entry.status).to_lowercase();
                        let summary = entry
                            .result
                            .as_ref()
                            .map(|r| r.summary.clone())
                            .unwrap_or_default();
                        let modified_files = entry
                            .result
                            .as_ref()
                            .and_then(|r| r.modified_files.clone())
                            .unwrap_or_default();
                        let errors = entry.result.as_ref().and_then(|r| r.errors.clone());
                        matched.push(WorkerCompletionResult {
                            task_id: task_id.clone(),
                            worker: entry.worker.clone(),
                            status,
                            summary,
                            modified_files,
                            errors,
                        });
                    }
                }
            }
        }

        self.queue = remaining;
        matched
    }

    pub fn is_target_satisfied(
        batch: &DispatchBatch,
        target_ids: Option<&HashSet<String>>,
    ) -> bool {
        match target_ids {
            None => batch.is_all_completed(),
            Some(ids) => ids.iter().all(|id| {
                batch
                    .get_entry(id)
                    .is_some_and(|e| e.status.is_terminal())
            }),
        }
    }

    pub fn get_pending_target_task_ids(
        batch: &DispatchBatch,
        target_ids: Option<&HashSet<String>>,
    ) -> Vec<String> {
        match target_ids {
            None => batch
                .entries()
                .iter()
                .filter(|e| !e.status.is_terminal())
                .map(|e| e.task_id.clone())
                .collect(),
            Some(ids) => ids
                .iter()
                .filter(|id| {
                    batch
                        .get_entry(id)
                        .map(|e| !e.status.is_terminal())
                        .unwrap_or(true)
                })
                .cloned()
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl Default for DispatchCompletionQueue {
    fn default() -> Self {
        Self::new()
    }
}
