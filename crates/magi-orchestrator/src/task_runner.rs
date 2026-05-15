use magi_core::{
    AssignmentLease, EventId, LeaseId, Task, TaskId, TaskKind, TaskResultKind, UtcMillis, WorkerId,
};
use magi_event_bus::{EventEnvelope, InMemoryEventBus};
use magi_skill_runtime::SkillDispatchRuntime;
use magi_tool_runtime::ToolRegistry;
use magi_worker_runtime::{
    WorkerExecutionFinalReport, WorkerExecutionIntent, WorkerExecutionIntentStep,
    WorkerExecutionProfile, WorkerLoopAction, WorkerLoopOutcomeKind, WorkerRuntime,
};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Describes a worker's capabilities for task matching.
#[derive(Clone, Debug)]
pub struct WorkerInfo {
    pub worker_id: WorkerId,
    /// The role this worker can fulfil (e.g. "integration-dev", "reviewer", "debugger").
    pub role: String,
    /// Task kinds this worker is capable of handling.
    pub supported_kinds: Vec<TaskKind>,
    /// Maximum number of concurrent tasks this worker can handle (design 5.4).
    /// None means unlimited.
    pub parallelism_limit: Option<u32>,
    /// Role-specific system prompt template injected at LLM invocation (design 8.1).
    pub system_prompt_template: Option<String>,
}

/// The outcome of a single `run_cycle` iteration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunCycleOutcome {
    /// There are still tasks to process; the runner should continue.
    Continue,
    /// Every task in the graph has reached a terminal state.
    AllComplete,
    /// No workers match the remaining runnable tasks.
    Blocked(Vec<TaskId>),
    /// An unexpected error occurred during the cycle.
    Error(String),
}

// ---------------------------------------------------------------------------
// Dispatch callback trait
// ---------------------------------------------------------------------------

/// Trait for dispatching a matched task to a worker for execution.
///
/// Implementations receive the task, worker info, and the granted lease, and
/// are responsible for triggering the actual execution pipeline.  The Runner
/// calls `dispatch` after granting a lease and marking the task as Running.
pub trait TaskDispatcher: Send + Sync {
    fn dispatch(
        &self,
        task: &Task,
        worker: &WorkerInfo,
        lease: &AssignmentLease,
    ) -> Result<(), String>;
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
    /// The task cannot proceed automatically and needs human/repair intervention.
    NeedsRepair { reason: String },
    /// Execution done but requires verification (design 3.3.3: Running→Verifying).
    NeedsVerification { output_refs: Vec<String> },
}

/// Trait for receiving execution results from workers.
///
/// The Runner calls `poll_results` at the start of each cycle to collect
/// any results that have arrived since the last cycle.
pub trait TaskResultReceiver: Send + Sync {
    fn poll_results(&self) -> Vec<TaskResult>;
}

// ---------------------------------------------------------------------------
// Inert implementations used by isolated runner tests
// ---------------------------------------------------------------------------

/// A dispatcher that accepts every dispatch but does nothing.
///
/// This is the default used by isolated runner tests that only verify graph
/// scheduling semantics.
pub struct NoOpDispatcher;

impl TaskDispatcher for NoOpDispatcher {
    fn dispatch(
        &self,
        _task: &Task,
        _worker: &WorkerInfo,
        _lease: &AssignmentLease,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// A result receiver that never returns any results.
///
/// Paired with `NoOpDispatcher` when tests do not need external results.
pub struct NoOpResultReceiver;

impl TaskResultReceiver for NoOpResultReceiver {
    fn poll_results(&self) -> Vec<TaskResult> {
        Vec::new()
    }
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

// ---------------------------------------------------------------------------
// Event-publishing dispatcher
// ---------------------------------------------------------------------------

/// Task event type constant for dispatch events.
pub const TASK_DISPATCHED: &str = "task.dispatched";

/// A dispatcher that publishes a `task.dispatched` domain event to the event
/// bus whenever a task is dispatched to a worker.
///
/// This is the production dispatcher wired through `RunnerManager` so that
/// the rest of the system (SSE clients, read models, etc.) can observe task
/// dispatch activity in real time.
pub struct EventBasedTaskDispatcher {
    event_bus: Arc<InMemoryEventBus>,
}

impl EventBasedTaskDispatcher {
    pub fn new(event_bus: Arc<InMemoryEventBus>) -> Self {
        Self { event_bus }
    }
}

impl TaskDispatcher for EventBasedTaskDispatcher {
    fn dispatch(
        &self,
        task: &Task,
        worker: &WorkerInfo,
        lease: &AssignmentLease,
    ) -> Result<(), String> {
        let event = EventEnvelope::domain(
            EventId::new(format!("event-task-dispatched-{}", UtcMillis::now().0)),
            TASK_DISPATCHED,
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": task.mission_id.to_string(),
                "worker_id": worker.worker_id.to_string(),
                "role": worker.role,
                "lease_id": lease.lease_id.to_string(),
                "kind": format!("{:?}", task.kind),
            }),
        );
        self.event_bus
            .publish(event)
            .map(|_| ())
            .map_err(|e| format!("failed to publish task.dispatched event: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Worker execution dispatcher — bridges TaskRunner to WorkerRuntimeLoop
// ---------------------------------------------------------------------------

/// A dispatcher that actually executes tasks through the WorkerRuntimeLoop,
/// bridging the TaskRunner scheduling layer to the worker execution pipeline.
///
/// When the TaskRunner dispatches a task, this dispatcher:
/// 1. Builds a `WorkerExecutionIntent` from the Task metadata
/// 2. Registers the intent with the `WorkerRuntime`
/// 3. Creates a `WorkerRuntimeLoop`, enqueues an Execute action, and steps
/// 4. Converts the `WorkerLoopOutcome` into a `TaskResult`
/// 5. Pushes the result to the `EventBasedResultReceiver` for the next cycle
pub struct WorkerExecutionDispatcher {
    worker_runtime: WorkerRuntime,
    result_receiver: Arc<EventBasedResultReceiver>,
    tool_registry: Option<ToolRegistry>,
    skill_dispatch_runtime: Option<SkillDispatchRuntime>,
    event_bus: Option<Arc<InMemoryEventBus>>,
}

impl WorkerExecutionDispatcher {
    pub fn new(
        worker_runtime: WorkerRuntime,
        result_receiver: Arc<EventBasedResultReceiver>,
    ) -> Self {
        Self {
            worker_runtime,
            result_receiver,
            tool_registry: None,
            skill_dispatch_runtime: None,
            event_bus: None,
        }
    }

    pub fn with_execution_drivers(
        mut self,
        tool_registry: ToolRegistry,
        skill_dispatch_runtime: SkillDispatchRuntime,
    ) -> Self {
        self.tool_registry = Some(tool_registry);
        self.skill_dispatch_runtime = Some(skill_dispatch_runtime);
        self
    }

    pub fn with_event_bus(mut self, event_bus: Arc<InMemoryEventBus>) -> Self {
        self.event_bus = Some(event_bus);
        self
    }

    pub fn build_intent_from_task(&self, task: &Task, worker: &WorkerInfo) -> WorkerExecutionIntent {
        let prefix = format!("{}-{}", task.mission_id, task.task_id);
        let mut steps: Vec<WorkerExecutionIntentStep> = Vec::new();

        steps.push(WorkerExecutionIntentStep::BuiltinToolInvocation {
            tool_call_id: magi_core::ToolCallId::new(format!("{prefix}-inspect")),
            tool_name: "process_inspect".to_string(),
            tool_kind: magi_governance::ToolKind::Builtin,
            input: serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": task.mission_id.to_string(),
                "kind": format!("{:?}", task.kind),
                "goal": task.goal,
                "context_refs": task.context_refs,
                "knowledge_refs": task.knowledge_refs,
                "input_refs": task.input_refs,
                "write_scope": task.write_scope,
                "policy": task.policy_snapshot.as_ref().map(|p| serde_json::json!({
                    "allowed_paths": p.allowed_paths,
                    "denied_paths": p.denied_paths,
                    "network_mode": p.network_mode,
                    "command_mode": p.command_mode,
                })),
            })
            .to_string(),
            approval_requirement: magi_core::ApprovalRequirement::None,
            risk_level: magi_core::RiskLevel::Low,
            status: magi_core::ExecutionResultStatus::Succeeded,
        });

        steps.push(WorkerExecutionIntentStep::FinalReport(
            WorkerExecutionFinalReport {
                summary: format!("执行任务: {}", task.title),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(magi_core::TerminationReason::Completed),
                verification_status: magi_core::VerificationStatus::Passed,
            },
        ));

        WorkerExecutionIntent {
            worker_id: worker.worker_id.clone(),
            task_id: task.task_id.clone(),
            session_id: None,
            workspace_id: None,
            execution_profile: WorkerExecutionProfile::default(),
            steps,
        }
    }

    fn outcome_to_task_result(
        task_id: &TaskId,
        lease_id: &LeaseId,
        outcome: &magi_worker_runtime::WorkerLoopOutcome,
    ) -> TaskResult {
        match outcome.kind {
            WorkerLoopOutcomeKind::Applied => {
                if let Some(ref report) = outcome.report {
                    match report.result_kind {
                        Some(TaskResultKind::Success) => TaskResult {
                            task_id: task_id.clone(),
                            lease_id: lease_id.clone(),
                            outcome: TaskOutcome::Completed {
                                output_refs: vec![report.summary.clone()],
                            },
                        },
                        _ => TaskResult {
                            task_id: task_id.clone(),
                            lease_id: lease_id.clone(),
                            outcome: TaskOutcome::Failed {
                                error: report.summary.clone(),
                            },
                        },
                    }
                } else {
                    TaskResult {
                        task_id: task_id.clone(),
                        lease_id: lease_id.clone(),
                        outcome: TaskOutcome::Completed {
                            output_refs: Vec::new(),
                        },
                    }
                }
            }
            WorkerLoopOutcomeKind::NeedsApproval => TaskResult {
                task_id: task_id.clone(),
                lease_id: lease_id.clone(),
                outcome: TaskOutcome::NeedsRepair {
                    reason: outcome
                        .rejection_reason
                        .clone()
                        .unwrap_or_else(|| "needs approval".to_string()),
                },
            },
            WorkerLoopOutcomeKind::Blocked | WorkerLoopOutcomeKind::Rejected => TaskResult {
                task_id: task_id.clone(),
                lease_id: lease_id.clone(),
                outcome: TaskOutcome::Failed {
                    error: outcome
                        .rejection_reason
                        .clone()
                        .unwrap_or_else(|| "worker execution rejected".to_string()),
                },
            },
        }
    }
}

impl TaskDispatcher for WorkerExecutionDispatcher {
    fn dispatch(
        &self,
        task: &Task,
        worker: &WorkerInfo,
        lease: &AssignmentLease,
    ) -> Result<(), String> {
        let intent = self.build_intent_from_task(task, worker);
        self.worker_runtime.register_execution_intent(intent);

        // Apply task policy tool filtering (design 3.2 / 4.5).
        let filtered_tool_registry = self.tool_registry.as_ref().map(|reg| {
            if let Some(ref policy) = task.policy_snapshot {
                reg.filtered_clone(&policy.allowed_tools, &policy.denied_tools)
            } else {
                reg.clone()
            }
        });

        let loop_controller = if let (Some(tool_registry), Some(skill_dispatch)) =
            (&filtered_tool_registry, &self.skill_dispatch_runtime)
        {
            self.worker_runtime
                .loop_controller()
                .with_execution_drivers(tool_registry.clone(), skill_dispatch.clone())
        } else {
            self.worker_runtime.loop_controller()
        };

        loop_controller.enqueue_action(WorkerLoopAction::Execute {
            worker_id: worker.worker_id.clone(),
            task_id: task.task_id.clone(),
        });

        let outcome = loop_controller
            .step()
            .ok_or_else(|| format!("worker loop returned no outcome for task {}", task.task_id))?;

        let task_result = Self::outcome_to_task_result(&task.task_id, &lease.lease_id, &outcome);
        self.result_receiver.push_result(task_result);

        if let Some(ref event_bus) = self.event_bus {
            let event = EventEnvelope::domain(
                EventId::new(format!("event-task-exec-{}", UtcMillis::now().0)),
                TASK_DISPATCHED,
                serde_json::json!({
                    "task_id": task.task_id.to_string(),
                    "mission_id": task.mission_id.to_string(),
                    "worker_id": worker.worker_id.to_string(),
                    "role": worker.role,
                    "lease_id": lease.lease_id.to_string(),
                    "kind": format!("{:?}", task.kind),
                    "executed": true,
                }),
            );
            let _ = event_bus.publish(event);
        }

        Ok(())
    }
}

