use std::collections::HashSet;

use super::completion::{DispatchCompletionQueue, WorkerCompletionResult};

pub struct DispatchReactiveWaitCoordinator {
    completion_queue: DispatchCompletionQueue,
    reactive_mode: bool,
    batches_awaiting_outcome: HashSet<String>,
}

impl DispatchReactiveWaitCoordinator {
    pub fn new() -> Self {
        Self {
            completion_queue: DispatchCompletionQueue::new(),
            reactive_mode: false,
            batches_awaiting_outcome: HashSet::new(),
        }
    }

    pub fn reset_for_new_execution_cycle(&mut self, active_batch_id: Option<&str>) {
        if let Some(batch_id) = active_batch_id {
            self.batches_awaiting_outcome.remove(batch_id);
        }
        self.reactive_mode = false;
        self.completion_queue.reset();
    }

    pub fn reset_for_next_batch(&mut self) {
        self.completion_queue.reset();
    }

    pub fn push_completion(
        &mut self,
        task_id: &str,
        worker: &str,
        status: &str,
        summary: &str,
        modified_files: Vec<String>,
        errors: Option<Vec<String>>,
    ) {
        self.completion_queue
            .push(task_id, worker, status, summary, modified_files, errors);
    }

    pub fn is_reactive_mode(&self) -> bool {
        self.reactive_mode
    }

    pub fn mark_batch_awaiting_outcome(&mut self, batch_id: &str) {
        self.batches_awaiting_outcome.insert(batch_id.to_string());
    }

    pub fn clear_batch_awaiting_outcome(&mut self, batch_id: &str) {
        self.batches_awaiting_outcome.remove(batch_id);
    }

    pub fn is_batch_awaiting_outcome(&self, batch_id: &str) -> bool {
        self.batches_awaiting_outcome.contains(batch_id)
    }

    pub fn mark_batch_outcome_published(&mut self, batch_id: &str) {
        self.batches_awaiting_outcome.remove(batch_id);
    }

    pub fn enter_reactive_mode(&mut self) {
        self.reactive_mode = true;
    }

    pub fn drain_completed(&mut self) -> Vec<WorkerCompletionResult> {
        self.completion_queue.drain_all()
    }

    pub fn dispose(&mut self) {
        self.completion_queue.reset();
        self.reactive_mode = false;
        self.batches_awaiting_outcome.clear();
    }
}

impl Default for DispatchReactiveWaitCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactive_mode_toggle() {
        let mut coord = DispatchReactiveWaitCoordinator::new();
        assert!(!coord.is_reactive_mode());
        coord.enter_reactive_mode();
        assert!(coord.is_reactive_mode());
        coord.reset_for_new_execution_cycle(None);
        assert!(!coord.is_reactive_mode());
    }

    #[test]
    fn batch_awaiting_outcome_lifecycle() {
        let mut coord = DispatchReactiveWaitCoordinator::new();
        assert!(!coord.is_batch_awaiting_outcome("b1"));
        coord.mark_batch_awaiting_outcome("b1");
        assert!(coord.is_batch_awaiting_outcome("b1"));
        coord.mark_batch_outcome_published("b1");
        assert!(!coord.is_batch_awaiting_outcome("b1"));
    }

    #[test]
    fn push_and_drain_completions() {
        let mut coord = DispatchReactiveWaitCoordinator::new();
        coord.push_completion("t1", "w1", "completed", "ok", vec![], None);
        coord.push_completion("t2", "w1", "completed", "ok", vec![], None);
        let drained = coord.drain_completed();
        assert_eq!(drained.len(), 2);
        assert!(coord.drain_completed().is_empty());
    }

    #[test]
    fn dispose_clears_all() {
        let mut coord = DispatchReactiveWaitCoordinator::new();
        coord.enter_reactive_mode();
        coord.mark_batch_awaiting_outcome("b1");
        coord.push_completion("t1", "w1", "completed", "ok", vec![], None);
        coord.dispose();
        assert!(!coord.is_reactive_mode());
        assert!(!coord.is_batch_awaiting_outcome("b1"));
        assert!(coord.drain_completed().is_empty());
    }

    #[test]
    fn reset_for_new_cycle_clears_specific_batch() {
        let mut coord = DispatchReactiveWaitCoordinator::new();
        coord.mark_batch_awaiting_outcome("b1");
        coord.mark_batch_awaiting_outcome("b2");
        coord.reset_for_new_execution_cycle(Some("b1"));
        assert!(!coord.is_batch_awaiting_outcome("b1"));
        assert!(coord.is_batch_awaiting_outcome("b2"));
    }
}
