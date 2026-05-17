//! Task System v2 — TaskRunner 执行桥层。
//!
//! WorkerInfo 仍由 magi_orchestrator::task_worker_catalog 提供；本模块持有调度 trait、
//! 结果接收器、event-based dispatcher 与 WorkerRuntime dispatcher。

use magi_core::{LeaseId, Task, TaskId};
use magi_orchestrator::task_store::TaskLease;
use magi_orchestrator::task_worker_catalog::WorkerInfo;
use std::collections::HashSet;
use std::sync::Mutex;

/// The outcome of a single `run_cycle` iteration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunCycleOutcome {
    /// There are still tasks to process; the runner should continue.
    Continue,
    /// Every task in the graph has reached a terminal state.
    AllComplete,
    /// Dispatch is intentionally paused by a non-terminal runtime gate.
    Blocked {
        task_ids: Vec<TaskId>,
        reason: String,
    },
    /// No runnable task can currently be dispatched.
    Stalled(Vec<TaskId>),
    /// An unexpected error occurred during the cycle.
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskDispatchGateDecision {
    Allow,
    Blocked(String),
}

pub type TaskDispatchGate = dyn Fn(&Task) -> Result<TaskDispatchGateDecision, String> + Send + Sync;

// ---------------------------------------------------------------------------
// Dispatch callback trait
// ---------------------------------------------------------------------------

/// Trait for dispatching a matched task to a worker for execution.
///
/// Implementations receive the task, worker info, and the granted lease, and
/// are responsible for triggering the actual execution pipeline.  The Runner
/// calls `dispatch` after granting a lease and marking the task as Running.
pub trait TaskDispatcher: Send + Sync {
    fn dispatch(&self, task: &Task, worker: &WorkerInfo, lease: &TaskLease) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// Result receiver trait
// ---------------------------------------------------------------------------

/// The outcome of a single task execution, reported back to the Runner.
#[derive(Clone, Debug)]
pub struct TaskResult {
    pub task_id: TaskId,
    pub lease_id: LeaseId,
    pub outcome: TaskOutcome,
}

/// Possible outcomes when a dispatched task finishes.
#[derive(Clone, Debug)]
pub enum TaskOutcome {
    /// Execution succeeded.  `output_refs` lists any produced artefacts.
    Completed { output_refs: Vec<String> },
    /// Execution failed with the given error description.
    Failed { error: String },
}

/// Trait for receiving execution results from workers.
///
/// The Runner calls `poll_results` at the start of each cycle to collect
/// any results that have arrived since the last cycle.
pub trait TaskResultReceiver: Send + Sync {
    fn poll_results(&self) -> Vec<TaskResult>;
}

// ---------------------------------------------------------------------------
// Event-based result receiver
// ---------------------------------------------------------------------------

/// A result receiver that collects results pushed externally (e.g. from a
/// `StatusChangeCallback` on the TaskStore) and returns them when polled.
///
/// This is the production receiver wired through `RunnerManager`.  When a
/// task transitions to a terminal state (Completed/Failed) the status-change
/// callback pushes a `TaskResult` into this receiver so the Runner's
/// `apply_results` step can process it on the next cycle.
///
/// Results are deduplicated by task ID: once a result for a given task has been
/// pushed, subsequent pushes for the same task are silently ignored.  This
/// prevents feedback loops when the Runner's own `apply_results` re-applies
/// the terminal status via `update_status`, which would otherwise re-trigger
/// the status-change callback.  Call `clear_seen` when a task is reset to a
/// non-terminal state (e.g. Ready) to allow future results for that task.
pub struct EventBasedResultReceiver {
    results: Mutex<Vec<TaskResult>>,
    seen: Mutex<HashSet<TaskId>>,
}

impl EventBasedResultReceiver {
    pub fn new() -> Self {
        Self {
            results: Mutex::new(Vec::new()),
            seen: Mutex::new(HashSet::new()),
        }
    }

    /// Push a result into the buffer.  Called from the TaskStore's
    /// `StatusChangeCallback` when a task reaches a terminal state.
    ///
    /// If a result for this `task_id` has already been pushed, the call is a
    /// no-op — this prevents feedback loops.
    pub fn push_result(&self, result: TaskResult) {
        let mut seen = self
            .seen
            .lock()
            .expect("EventBasedResultReceiver seen lock poisoned");
        if !seen.insert(result.task_id.clone()) {
            // Already pushed a result for this task — skip.
            return;
        }
        self.results
            .lock()
            .expect("EventBasedResultReceiver results lock poisoned")
            .push(result);
    }

    /// Clear the deduplication entry for a task.  Call this when a task is
    /// reset to a non-terminal state (e.g. Ready after a lease expiry) so
    /// that a future terminal transition can produce a new result.
    pub fn clear_seen(&self, task_id: &TaskId) {
        self.seen
            .lock()
            .expect("EventBasedResultReceiver seen lock poisoned")
            .remove(task_id);
    }
}

impl TaskResultReceiver for EventBasedResultReceiver {
    fn poll_results(&self) -> Vec<TaskResult> {
        let mut guard = self
            .results
            .lock()
            .expect("EventBasedResultReceiver results lock poisoned");
        std::mem::take(&mut *guard)
    }
}
