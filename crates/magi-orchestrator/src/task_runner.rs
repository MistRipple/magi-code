use crate::task_store::TaskStore;
use crate::task_worker_catalog::resolve_task_role;
use magi_bridge_client::{ChatMessage, ModelBridgeClient, ModelInvocationRequest};
use magi_core::{
    AssignmentLease, DecisionOption, DecisionTaskPayload, EventId, LeaseId, Task, TaskId, TaskKind,
    TaskResultKind, TaskStatus, UtcMillis, WorkerId,
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

    fn build_intent_from_task(&self, task: &Task, worker: &WorkerInfo) -> WorkerExecutionIntent {
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
            })
            .to_string(),
            approval_requirement: magi_core::ApprovalRequirement::None,
            risk_level: magi_core::RiskLevel::Low,
            status: magi_core::ExecutionResultStatus::Succeeded,
        });

        steps.push(WorkerExecutionIntentStep::FinalReport(
            WorkerExecutionFinalReport {
                summary: format!("执行任务: {}", task.title),
                result_kind: None,
                termination_reason: None,
                verification_status: magi_core::VerificationStatus::Pending,
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

        let loop_controller = if let (Some(tool_registry), Some(skill_dispatch)) =
            (&self.tool_registry, &self.skill_dispatch_runtime)
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

/// Default lease duration in milliseconds (60 seconds).
const DEFAULT_LEASE_DURATION_MS: u64 = 60_000;

/// The task runner implements the main scheduling loop for the Task Graph
/// orchestration system. Each call to `run_cycle` performs one iteration:
///
/// 1. Poll the result receiver and apply any completed/failed results.
/// 2. Collect and revoke expired leases, resetting tasks to Ready.
/// 3. Propagate parent completion when all children are done.
/// 4. Compute runnable leaf tasks.
/// 5. Match workers to runnable tasks by kind and role.
/// 6. Grant leases, mark matched tasks as Running, and call the dispatcher.
/// 7. Evaluate termination conditions.
pub struct TaskRunner {
    store: Arc<TaskStore>,
    workers: Vec<WorkerInfo>,
    dispatcher: Arc<dyn TaskDispatcher>,
    result_receiver: Arc<dyn TaskResultReceiver>,
    decomposer: Option<Arc<dyn MissionDecomposer>>,
    reflector: Arc<dyn GraphReflector>,
}

impl TaskRunner {
    /// Create a runner with an inert dispatcher and result receiver.
    ///
    /// Production daemon wiring uses `with_dispatcher`; this constructor keeps
    /// unit tests focused on task-graph scheduling without external runtime IO.
    pub fn new(store: Arc<TaskStore>, workers: Vec<WorkerInfo>) -> Self {
        Self {
            store,
            workers,
            dispatcher: Arc::new(NoOpDispatcher),
            result_receiver: Arc::new(NoOpResultReceiver),
            decomposer: None,
            reflector: Arc::new(DefaultGraphReflector),
        }
    }

    /// Create a runner wired to real dispatch and result-collection
    /// implementations.
    pub fn with_dispatcher(
        store: Arc<TaskStore>,
        workers: Vec<WorkerInfo>,
        dispatcher: Arc<dyn TaskDispatcher>,
        result_receiver: Arc<dyn TaskResultReceiver>,
    ) -> Self {
        Self {
            store,
            workers,
            dispatcher,
            result_receiver,
            decomposer: None,
            reflector: Arc::new(DefaultGraphReflector),
        }
    }

    /// Attach a mission decomposer for LLM-based task graph generation.
    pub fn with_decomposer(mut self, decomposer: Arc<dyn MissionDecomposer>) -> Self {
        self.decomposer = Some(decomposer);
        self
    }

    /// Replace the default graph reflector.
    pub fn with_reflector(mut self, reflector: Arc<dyn GraphReflector>) -> Self {
        self.reflector = reflector;
        self
    }

    /// Access the graph reflector for runtime graph mutations.
    pub fn reflector(&self) -> &dyn GraphReflector {
        self.reflector.as_ref()
    }

    /// Decompose a mission goal into a task graph and insert it into the store.
    pub fn decompose_mission(
        &self,
        mission_id: &magi_core::MissionId,
        root_task_id: &TaskId,
        goal: &str,
    ) -> Result<Vec<TaskId>, String> {
        let decomposer = self
            .decomposer
            .as_ref()
            .ok_or_else(|| "MissionDecomposer 未配置".to_string())?;
        let tasks = decomposer.decompose(mission_id, root_task_id, goal)?;
        let ids: Vec<TaskId> = tasks.iter().map(|t| t.task_id.clone()).collect();
        for task in tasks {
            self.store.insert_task(task);
        }
        Ok(ids)
    }

    /// Run one scheduling cycle for the task graph rooted at `root_task_id`.
    ///
    /// Returns a `RunCycleOutcome` indicating whether the runner should
    /// continue, has completed, is blocked, or encountered an error.
    pub fn run_cycle(&self, root_task_id: &TaskId) -> RunCycleOutcome {
        // Step 0: Poll for execution results and apply them.
        if let Err(e) = self.apply_results() {
            return RunCycleOutcome::Error(e);
        }

        // Step 1: Collect and handle expired leases
        let expired = self.store.collect_expired_leases();
        for (task_id, lease_id) in &expired {
            self.store.revoke_lease(task_id, lease_id);
            // Reset the task back to Ready so it can be re-dispatched.
            if let Err(e) = self.store.update_status(task_id, TaskStatus::Ready) {
                return RunCycleOutcome::Error(format!(
                    "failed to reset expired-lease task {task_id}: {e}"
                ));
            }
        }

        // Step 1.5: Heartbeat all active leases to prevent premature expiry.
        let active_leases = self.store.collect_active_leases();
        for (task_id, lease_id) in &active_leases {
            self.store.heartbeat_lease(task_id, lease_id);
        }

        // Step 2: Propagate parent completion.
        if let Err(e) = self.propagate_parent_completion(root_task_id) {
            return RunCycleOutcome::Error(e);
        }

        // Step 3: Check termination — are all tasks in a terminal state?
        if self.all_tasks_terminal(root_task_id) {
            return RunCycleOutcome::AllComplete;
        }

        // Step 4: Compute runnable leaves.
        let runnable = self.store.get_runnable_leaves(root_task_id);
        if runnable.is_empty() {
            // Nothing runnable but not all complete — we're blocked.
            let blocked_ids = self.collect_non_terminal_task_ids(root_task_id);
            return RunCycleOutcome::Blocked(blocked_ids);
        }

        // Step 5: Match workers to runnable tasks and dispatch.
        let mut dispatched = 0usize;
        let mut unmatched_ids: Vec<TaskId> = Vec::new();

        // Collect write scopes of currently Running tasks for conflict detection.
        let mut running_write_scopes = self.collect_running_write_scopes(root_task_id);
        // Collect parallelism_groups of currently Running tasks (design 5.3).
        let mut running_parallelism_groups = self.collect_running_parallelism_groups(root_task_id);

        for task in &runnable {
            // Decision tasks are never dispatched to workers — they wait for
            // human input.
            if task.kind == TaskKind::Decision {
                unmatched_ids.push(task.task_id.clone());
                continue;
            }

            // Write scope conflict check: skip task if it would conflict
            // with a currently running task.
            if let Some(ref scope) = task.write_scope {
                if running_write_scopes
                    .iter()
                    .any(|rs| scopes_conflict(rs, scope))
                {
                    unmatched_ids.push(task.task_id.clone());
                    continue;
                }
            }

            // Exclusive scope conflict check via executor_binding.
            if let Some(ref binding) = task.executor_binding {
                if let Some(ref exc_scope) = binding.exclusive_scope {
                    if self.has_running_exclusive_scope(root_task_id, exc_scope, &task.task_id) {
                        unmatched_ids.push(task.task_id.clone());
                        continue;
                    }
                }
                // Parallelism group conflict: same group cannot run concurrently (design 5.3).
                if let Some(ref group) = binding.parallelism_group {
                    if running_parallelism_groups.contains(group) {
                        unmatched_ids.push(task.task_id.clone());
                        continue;
                    }
                }
            }

            // Policy snapshot validation: reject tasks without a frozen policy
            // if the policy is required (non-structural tasks).
            if matches!(
                task.kind,
                TaskKind::Action | TaskKind::Validation | TaskKind::Repair
            ) && task.policy_snapshot.is_some()
            {
                let policy = task.policy_snapshot.as_ref().unwrap();
                if !self.check_policy_allows_dispatch(policy, task) {
                    unmatched_ids.push(task.task_id.clone());
                    continue;
                }
            }

            let Some(required_role) = resolve_task_role(task) else {
                unmatched_ids.push(task.task_id.clone());
                continue;
            };

            let matched_worker = self
                .workers
                .iter()
                .find(|w| w.role == required_role && w.supported_kinds.contains(&task.kind));

            if let Some(worker) = matched_worker {
                let latest_task = match self.store.get_task(&task.task_id) {
                    Some(latest_task) => latest_task,
                    None => {
                        unmatched_ids.push(task.task_id.clone());
                        continue;
                    }
                };
                if latest_task.status != TaskStatus::Ready {
                    unmatched_ids.push(task.task_id.clone());
                    continue;
                }
                // Worker parallelism_limit check (design 5.4).
                if let Some(limit) = worker.parallelism_limit {
                    let active_count =
                        self.store.get_leases_by_worker(&worker.worker_id).len() as u32;
                    if active_count >= limit {
                        unmatched_ids.push(task.task_id.clone());
                        continue;
                    }
                }
                // Grant a lease.
                let lease = self.store.grant_lease(
                    &task.task_id,
                    &worker.worker_id,
                    &worker.role,
                    DEFAULT_LEASE_DURATION_MS,
                );
                if let Some(ref granted_lease) = lease {
                    let latest_task = match self.store.get_task(&task.task_id) {
                        Some(latest_task) => latest_task,
                        None => {
                            self.store
                                .revoke_lease(&task.task_id, &granted_lease.lease_id);
                            unmatched_ids.push(task.task_id.clone());
                            continue;
                        }
                    };
                    if latest_task.status != TaskStatus::Ready {
                        self.store
                            .revoke_lease(&task.task_id, &granted_lease.lease_id);
                        unmatched_ids.push(task.task_id.clone());
                        continue;
                    }
                    // Mark task as Running.
                    if let Err(e) = self.store.update_status(&task.task_id, TaskStatus::Running) {
                        return RunCycleOutcome::Error(format!(
                            "failed to mark task {} as Running: {e}",
                            task.task_id
                        ));
                    }
                    // Invoke the dispatcher to trigger actual execution.
                    if let Err(e) = self.dispatcher.dispatch(task, worker, granted_lease) {
                        return RunCycleOutcome::Error(format!(
                            "dispatcher failed for task {}: {e}",
                            task.task_id
                        ));
                    }
                    dispatched += 1;
                    if let Some(ref scope) = task.write_scope {
                        running_write_scopes.push(scope.clone());
                    }
                    if let Some(ref binding) = task.executor_binding {
                        if let Some(ref group) = binding.parallelism_group {
                            running_parallelism_groups.insert(group.clone());
                        }
                    }
                }
                // If grant_lease returns None the task already has an active
                // lease — skip it silently.
            } else {
                unmatched_ids.push(task.task_id.clone());
            }
        }

        if dispatched == 0 && !unmatched_ids.is_empty() {
            return RunCycleOutcome::Blocked(unmatched_ids);
        }

        RunCycleOutcome::Continue
    }

    /// Poll the result receiver and apply each result to the task store.
    fn apply_results(&self) -> Result<(), String> {
        let results = self.result_receiver.poll_results();
        for result in results {
            if !self.store.complete_lease(&result.task_id, &result.lease_id) {
                tracing::warn!(
                    task_id = %result.task_id,
                    lease_id = %result.lease_id,
                    "ignore stale task result because lease is no longer active"
                );
                continue;
            }

            let next_status = match &result.outcome {
                TaskOutcome::Completed { output_refs } => {
                    if !output_refs.is_empty() {
                        self.store
                            .set_output_refs(&result.task_id, output_refs.clone());
                    }
                    // If task has a validation_profile, route through Verifying (design 3.3.3).
                    if let Some(task) = self.store.get_task(&result.task_id) {
                        if task
                            .policy_snapshot
                            .as_ref()
                            .and_then(|p| p.validation_profile.as_ref())
                            .is_some()
                        {
                            TaskStatus::Verifying
                        } else {
                            TaskStatus::Completed
                        }
                    } else {
                        TaskStatus::Completed
                    }
                }
                TaskOutcome::NeedsVerification { output_refs } => {
                    if !output_refs.is_empty() {
                        self.store
                            .set_output_refs(&result.task_id, output_refs.clone());
                    }
                    TaskStatus::Verifying
                }
                TaskOutcome::Failed { error } => {
                    tracing::error!(task_id = %result.task_id, %error, "task execution failed");
                    self.store.set_output_refs(
                        &result.task_id,
                        vec![format!("{{\"error\":\"{}\"}}", error.replace('"', "\\\""))],
                    );
                    TaskStatus::Failed
                }
                TaskOutcome::NeedsRepair { reason } => {
                    // Check repair budget before creating Repair task.
                    if let Some(task) = self.store.get_task(&result.task_id) {
                        let repair_limit = task
                            .policy_snapshot
                            .as_ref()
                            .map(|p| p.repair_limit)
                            .unwrap_or(3);
                        if task.repair_count < repair_limit {
                            // Increment repair_count on the original task.
                            self.store.increment_repair_count(&result.task_id);
                            // Create a Repair child task.
                            let repair_task = Task {
                                task_id: TaskId::new(format!(
                                    "{}-repair-{}",
                                    result.task_id,
                                    task.repair_count + 1
                                )),
                                mission_id: task.mission_id.clone(),
                                root_task_id: task.root_task_id.clone(),
                                parent_task_id: Some(task.task_id.clone()),
                                kind: TaskKind::Repair,
                                title: format!("Repair: {}", task.title),
                                goal: format!("修复失败: {}", reason),
                                status: TaskStatus::Ready,
                                dependency_ids: Vec::new(),
                                required_children: Vec::new(),
                                policy_snapshot: task.policy_snapshot.clone(),
                                executor_binding: task.executor_binding.clone(),
                                context_refs: task.context_refs.clone(),
                                knowledge_refs: task.knowledge_refs.clone(),
                                workspace_scope: task.workspace_scope.clone(),
                                write_scope: task.write_scope.clone(),
                                input_refs: vec![format!(
                                    "repair://task/{}/reason/{}",
                                    result.task_id, reason
                                )],
                                output_refs: Vec::new(),
                                evidence_refs: Vec::new(),
                                retry_count: 0,
                                repair_count: 0,
                                decision_payload: None,
                                created_at: UtcMillis::now(),
                                updated_at: UtcMillis::now(),
                            };
                            self.store.insert_task(repair_task);
                            TaskStatus::Repairing
                        } else {
                            tracing::warn!(
                                task_id = %result.task_id,
                                repair_count = task.repair_count,
                                repair_limit,
                                "repair budget exhausted, marking as Failed"
                            );
                            TaskStatus::Failed
                        }
                    } else {
                        TaskStatus::Repairing
                    }
                }
            };
            self.store
                .update_status(&result.task_id, next_status)
                .map_err(|e| format!("failed to apply result for task {}: {e}", result.task_id))?;

            // Auto-create Validation child when entering Verifying state.
            if next_status == TaskStatus::Verifying {
                self.create_validation_child(&result.task_id);
            }

            // Evaluate escalation_conditions on failure.
            if next_status == TaskStatus::Failed {
                self.evaluate_escalation(&result.task_id);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Auto-create a Validation child task when a task enters Verifying state.
    fn create_validation_child(&self, task_id: &TaskId) {
        let Some(task) = self.store.get_task(task_id) else {
            return;
        };
        let validation_profile = task
            .policy_snapshot
            .as_ref()
            .and_then(|p| p.validation_profile.as_deref())
            .unwrap_or("standard")
            .to_string();
        let validation_task = Task {
            task_id: TaskId::new(format!("{}-validation-{}", task_id, UtcMillis::now().0)),
            mission_id: task.mission_id.clone(),
            root_task_id: task.root_task_id.clone(),
            parent_task_id: Some(task.task_id.clone()),
            kind: TaskKind::Validation,
            title: format!("Verify: {}", task.title),
            goal: format!(
                "验证任务 {} 的执行结果 (profile: {})",
                task.title, validation_profile
            ),
            status: TaskStatus::Ready,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: task.policy_snapshot.clone(),
            executor_binding: task.executor_binding.clone(),
            context_refs: task.context_refs.clone(),
            knowledge_refs: task.knowledge_refs.clone(),
            workspace_scope: task.workspace_scope.clone(),
            write_scope: None,
            input_refs: task.output_refs.clone(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        };
        self.store.insert_task(validation_task);
    }

    /// Evaluate escalation_conditions after a task fails.
    fn evaluate_escalation(&self, task_id: &TaskId) {
        let Some(task) = self.store.get_task(task_id) else {
            return;
        };
        let conditions = task
            .policy_snapshot
            .as_ref()
            .map(|p| &p.escalation_conditions)
            .cloned()
            .unwrap_or_default();
        if conditions.is_empty() {
            return;
        }
        let should_escalate = conditions
            .iter()
            .any(|c| c == "on_failure" || c == "high_risk" || c == "on_repair_exhausted");
        if !should_escalate {
            return;
        }
        let Some(parent_id) = &task.parent_task_id else {
            return;
        };
        let payload = DecisionTaskPayload {
            decision_context: format!("任务 {} 执行失败，需要决策后续操作", task.title),
            blocked_reason: format!("任务 {} 失败 (escalation: {:?})", task_id, conditions),
            options: vec![
                DecisionOption {
                    option_id: "retry".to_string(),
                    label: "重试".to_string(),
                    description: "重新执行失败的任务".to_string(),
                },
                DecisionOption {
                    option_id: "skip".to_string(),
                    label: "跳过".to_string(),
                    description: "跳过此任务继续后续流程".to_string(),
                },
                DecisionOption {
                    option_id: "abort".to_string(),
                    label: "中止".to_string(),
                    description: "中止整个任务树".to_string(),
                },
            ],
            risk_notes: vec![format!("触发条件: {:?}", conditions)],
            recommended_option: Some("retry".to_string()),
            required_user_input: true,
            decision_evidence: None,
        };
        let _ = self.escalate_to_decision(parent_id, payload);
    }

    /// Walk the tree from `root_task_id` and propagate parent status:
    /// - All children terminal: parent → Completed/Failed
    /// - Any required child Blocked/AwaitingApproval: parent → Blocked
    /// - Otherwise: no change
    fn propagate_parent_completion(&self, root_task_id: &TaskId) -> Result<(), String> {
        loop {
            let all_ids = self.collect_all_task_ids(root_task_id);
            let mut changed = false;

            for task_id in &all_ids {
                let children = self.store.get_children(task_id);
                if children.is_empty() {
                    continue;
                }

                let task = match self.store.get_task(task_id) {
                    Some(t) => t,
                    None => continue,
                };

                if is_terminal(task.status) {
                    continue;
                }

                let required_children: Vec<&Task> = if task.required_children.is_empty() {
                    children.iter().collect()
                } else {
                    children
                        .iter()
                        .filter(|c| task.required_children.contains(&c.task_id))
                        .collect()
                };

                let all_terminal = required_children.iter().all(|c| is_terminal(c.status));

                if all_terminal && !required_children.is_empty() {
                    let any_failed = required_children
                        .iter()
                        .any(|c| c.status == TaskStatus::Failed);
                    let next_status = if any_failed {
                        TaskStatus::Failed
                    } else {
                        TaskStatus::Completed
                    };
                    self.store
                        .update_status(task_id, next_status)
                        .map_err(|e| {
                            format!("failed to propagate completion for {task_id}: {e}")
                        })?;
                    changed = true;
                    continue;
                }

                // Blocked/AwaitingApproval propagation: if any required child
                // is blocked, parent should reflect that.
                let any_blocked = required_children.iter().any(|c| {
                    c.status == TaskStatus::Blocked || c.status == TaskStatus::AwaitingApproval
                });
                if any_blocked && task.status != TaskStatus::Blocked {
                    self.store
                        .update_status(task_id, TaskStatus::Blocked)
                        .map_err(|e| format!("failed to propagate blocked for {task_id}: {e}"))?;
                    changed = true;
                }
            }

            if !changed {
                break;
            }
        }

        Ok(())
    }

    /// Returns `true` when every task in the tree is in a terminal state.
    fn all_tasks_terminal(&self, root_task_id: &TaskId) -> bool {
        let all_ids = self.collect_all_task_ids(root_task_id);
        all_ids.iter().all(|id| {
            self.store
                .get_task(id)
                .map(|t| is_terminal(t.status))
                .unwrap_or(true)
        })
    }

    /// Collect the IDs of all tasks in the tree that are NOT in a terminal
    /// state. Used to populate `RunCycleOutcome::Blocked`.
    fn collect_non_terminal_task_ids(&self, root_task_id: &TaskId) -> Vec<TaskId> {
        self.collect_all_task_ids(root_task_id)
            .into_iter()
            .filter(|id| {
                self.store
                    .get_task(id)
                    .map(|t| !is_terminal(t.status))
                    .unwrap_or(false)
            })
            .collect()
    }

    /// BFS to collect every task ID in the subtree rooted at `root_task_id`.
    fn collect_all_task_ids(&self, root_task_id: &TaskId) -> Vec<TaskId> {
        let mut all_ids: Vec<TaskId> = Vec::new();
        let mut queue: Vec<TaskId> = vec![root_task_id.clone()];
        while let Some(current) = queue.pop() {
            all_ids.push(current.clone());
            let children = self.store.get_children(&current);
            for child in children {
                queue.push(child.task_id);
            }
        }
        all_ids
    }

    /// Collect write_scope values of all currently Running tasks in the tree.
    fn collect_running_write_scopes(&self, root_task_id: &TaskId) -> Vec<String> {
        self.collect_all_task_ids(root_task_id)
            .into_iter()
            .filter_map(|id| {
                self.store.get_task(&id).and_then(|t| {
                    if t.status == TaskStatus::Running {
                        t.write_scope
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    /// Check if any Running task in the tree has the given exclusive_scope.
    fn has_running_exclusive_scope(
        &self,
        root_task_id: &TaskId,
        scope: &str,
        exclude_task_id: &TaskId,
    ) -> bool {
        self.collect_all_task_ids(root_task_id)
            .into_iter()
            .any(|id| {
                if id == *exclude_task_id {
                    return false;
                }
                self.store.get_task(&id).is_some_and(|t| {
                    t.status == TaskStatus::Running
                        && t.executor_binding
                            .as_ref()
                            .and_then(|b| b.exclusive_scope.as_deref())
                            == Some(scope)
                })
            })
    }

    /// Collect parallelism_group values from Running tasks (design 5.3).
    fn collect_running_parallelism_groups(&self, root_task_id: &TaskId) -> HashSet<String> {
        self.collect_all_task_ids(root_task_id)
            .into_iter()
            .filter_map(|id| {
                self.store.get_task(&id).and_then(|t| {
                    if t.status == TaskStatus::Running {
                        t.executor_binding
                            .as_ref()
                            .and_then(|b| b.parallelism_group.clone())
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    /// Check if the task's policy allows dispatch.
    fn check_policy_allows_dispatch(&self, policy: &magi_core::TaskPolicy, _task: &Task) -> bool {
        // Reject tasks that require manual approval but are in auto-dispatch flow.
        if policy.autonomy_level == "Manual" {
            return false;
        }
        true
    }

    // ------------------------------------------------------------------
    // G4: Decision Task lifecycle (design 7.x)
    // ------------------------------------------------------------------

    /// Create a Decision task as a child of the given task, blocking the parent
    /// until the decision is resolved (design 7.x).
    pub fn escalate_to_decision(
        &self,
        parent_task_id: &TaskId,
        payload: magi_core::DecisionTaskPayload,
    ) -> Result<TaskId, String> {
        let parent = self
            .store
            .get_task(parent_task_id)
            .ok_or_else(|| format!("parent task {parent_task_id} not found"))?;

        let decision_id = TaskId::new(format!(
            "{}-decision-{}",
            parent_task_id,
            UtcMillis::now().0
        ));
        let decision_task = Task {
            task_id: decision_id.clone(),
            mission_id: parent.mission_id.clone(),
            root_task_id: parent.root_task_id.clone(),
            parent_task_id: Some(parent_task_id.clone()),
            kind: TaskKind::Decision,
            title: format!("Decision: {}", payload.decision_context),
            goal: payload.blocked_reason.clone(),
            status: TaskStatus::AwaitingApproval,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: parent.policy_snapshot.clone(),
            executor_binding: None,
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: parent.workspace_scope.clone(),
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: Some(payload),
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        };
        self.store.insert_task(decision_task);
        self.store
            .update_status(parent_task_id, TaskStatus::Blocked)
            .map_err(|e| format!("failed to block parent {parent_task_id}: {e}"))?;

        Ok(decision_id)
    }

    /// Resolve a pending Decision task, completing it and unblocking its parent
    /// (design 7.x).
    pub fn resolve_decision(
        &self,
        decision_task_id: &TaskId,
        chosen_option: &str,
        evidence: Option<serde_json::Value>,
    ) -> Result<(), String> {
        self.store
            .resolve_decision(decision_task_id, chosen_option, evidence)
    }

    // ------------------------------------------------------------------
    // G5: Control commands (design 9.x)
    // ------------------------------------------------------------------

    /// Pause a running task and all its non-terminal可调度 descendants.
    pub fn pause_task(&self, task_id: &TaskId) -> Result<(), String> {
        let task = self.store.get_task(task_id).ok_or("task not found")?;
        if task.status != TaskStatus::Running {
            return Err(format!("cannot pause task in {:?} state", task.status));
        }
        self.store
            .update_status(task_id, TaskStatus::Blocked)
            .map_err(|e| format!("pause failed: {e}"))?;
        for descendant_id in self.collect_all_task_ids(task_id) {
            if descendant_id == *task_id {
                continue;
            }
            if self
                .store
                .get_task(&descendant_id)
                .is_some_and(|descendant| {
                    matches!(
                        descendant.status,
                        TaskStatus::Draft
                            | TaskStatus::Ready
                            | TaskStatus::Running
                            | TaskStatus::Verifying
                            | TaskStatus::Repairing
                    )
                })
            {
                let _ = self
                    .store
                    .update_status(&descendant_id, TaskStatus::Blocked);
            }
        }
        Ok(())
    }

    /// Resume a blocked/paused task and its blocked descendants.
    pub fn resume_task(&self, task_id: &TaskId) -> Result<(), String> {
        let task = self.store.get_task(task_id).ok_or("task not found")?;
        if task.status != TaskStatus::Blocked {
            return Err(format!("cannot resume task in {:?} state", task.status));
        }
        self.store
            .update_status(task_id, TaskStatus::Running)
            .map_err(|e| format!("resume failed: {e}"))?;
        for descendant_id in self.collect_all_task_ids(task_id) {
            if descendant_id == *task_id {
                continue;
            }
            if self
                .store
                .get_task(&descendant_id)
                .is_some_and(|descendant| descendant.status == TaskStatus::Blocked)
            {
                let _ = self.store.update_status(&descendant_id, TaskStatus::Ready);
            }
        }
        Ok(())
    }

    /// Cancel a task and all its descendants recursively.
    pub fn cancel_tree(&self, root_task_id: &TaskId) -> Result<u32, String> {
        let all_ids = self.collect_all_task_ids(root_task_id);
        let mut cancelled = 0u32;
        for id in &all_ids {
            if let Some(task) = self.store.get_task(id) {
                if !is_terminal(task.status) {
                    self.store
                        .update_status(id, TaskStatus::Cancelled)
                        .map_err(|e| format!("cancel failed for {id}: {e}"))?;
                    // Revoke any active lease.
                    if let Some(lease) = self.store.get_active_lease(id) {
                        self.store.revoke_lease(id, &lease.lease_id);
                    }
                    cancelled += 1;
                }
            }
        }
        Ok(cancelled)
    }

    /// Request a replan: cancel all non-terminal, non-completed tasks under root,
    /// returning the cancelled IDs so the caller can rebuild the graph (design 6.x).
    pub fn request_replan(&self, root_task_id: &TaskId) -> Result<Vec<TaskId>, String> {
        let all_ids = self.collect_all_task_ids(root_task_id);
        let mut cancelled_ids = Vec::new();
        for id in &all_ids {
            if id == root_task_id {
                continue; // Keep root alive.
            }
            if let Some(task) = self.store.get_task(id) {
                if !is_terminal(task.status) && task.status != TaskStatus::Completed {
                    self.store
                        .update_status(id, TaskStatus::Cancelled)
                        .map_err(|e| format!("replan cancel failed for {id}: {e}"))?;
                    if let Some(lease) = self.store.get_active_lease(id) {
                        self.store.revoke_lease(id, &lease.lease_id);
                    }
                    cancelled_ids.push(id.clone());
                }
            }
        }
        // Reset root to Running so new children can be added.
        let root = self.store.get_task(root_task_id);
        if let Some(r) = root {
            if r.status != TaskStatus::Running {
                let _ = self.store.update_status(root_task_id, TaskStatus::Running);
            }
        }
        Ok(cancelled_ids)
    }
}

// ---------------------------------------------------------------------------
// G10: Graph Reflection / Replanning trait (design 4.2.x / 6.x)
// ---------------------------------------------------------------------------

/// Trait for runtime graph mutations — adding/removing subtrees during execution.
pub trait GraphReflector: Send + Sync {
    /// Insert a new subtree under the given parent. Returns the IDs of newly
    /// added tasks.
    fn insert_subtree(
        &self,
        store: &TaskStore,
        parent_task_id: &TaskId,
        new_tasks: Vec<Task>,
    ) -> Result<Vec<TaskId>, String>;

    /// Remove a subtree rooted at `subtree_root_id`, cancelling all non-terminal
    /// tasks within it.
    fn remove_subtree(
        &self,
        store: &TaskStore,
        subtree_root_id: &TaskId,
    ) -> Result<Vec<TaskId>, String>;
}

/// Default graph reflector that directly manipulates the TaskStore.
pub struct DefaultGraphReflector;

impl GraphReflector for DefaultGraphReflector {
    fn insert_subtree(
        &self,
        store: &TaskStore,
        parent_task_id: &TaskId,
        new_tasks: Vec<Task>,
    ) -> Result<Vec<TaskId>, String> {
        if store.get_task(parent_task_id).is_none() {
            return Err(format!("parent {parent_task_id} not found"));
        }
        let mut ids = Vec::new();
        for task in new_tasks {
            ids.push(task.task_id.clone());
            store.insert_task(task);
        }
        Ok(ids)
    }

    fn remove_subtree(
        &self,
        store: &TaskStore,
        subtree_root_id: &TaskId,
    ) -> Result<Vec<TaskId>, String> {
        let mut queue = vec![subtree_root_id.clone()];
        let mut removed_ids = Vec::new();
        while let Some(id) = queue.pop() {
            let children = store.get_children(&id);
            for child in &children {
                queue.push(child.task_id.clone());
            }
            if let Some(task) = store.get_task(&id) {
                if !is_terminal(task.status) {
                    let _ = store.update_status(&id, TaskStatus::Cancelled);
                }
            }
            store.remove_task(&id);
            removed_ids.push(id);
        }
        Ok(removed_ids)
    }
}

// ---------------------------------------------------------------------------
// G10.1: LLM-based GraphReflector (design 9.1)
// ---------------------------------------------------------------------------

pub struct LlmGraphReflector {
    client: Arc<dyn ModelBridgeClient>,
    inner: DefaultGraphReflector,
}

impl LlmGraphReflector {
    pub fn new(client: Arc<dyn ModelBridgeClient>) -> Self {
        Self {
            client,
            inner: DefaultGraphReflector,
        }
    }

    fn build_replan_prompt(store: &TaskStore, parent_task_id: &TaskId) -> Option<String> {
        let parent = store.get_task(parent_task_id)?;
        let children = store.get_children(parent_task_id);

        let children_summary: Vec<String> = children
            .iter()
            .map(|c| {
                format!(
                    "  - {} (kind={:?}, status={:?}, goal={})",
                    c.task_id, c.kind, c.status, c.goal
                )
            })
            .collect();

        Some(format!(
            r#"当前任务图需要重规划。请分析以下父任务及其子任务，提出优化建议。

父任务: {} (kind={:?}, goal={})
当前子任务:
{}

请以 JSON 对象返回:
{{
  "action": "keep" | "replace",
  "reason": "重规划原因",
  "new_tasks": [仅当 action=replace 时提供新任务列表，格式同 MissionDecomposer]
}}

只返回 JSON，不要其他内容。"#,
            parent.task_id,
            parent.kind,
            parent.goal,
            children_summary.join("\n")
        ))
    }
}

impl GraphReflector for LlmGraphReflector {
    fn insert_subtree(
        &self,
        store: &TaskStore,
        parent_task_id: &TaskId,
        new_tasks: Vec<Task>,
    ) -> Result<Vec<TaskId>, String> {
        self.inner.insert_subtree(store, parent_task_id, new_tasks)
    }

    fn remove_subtree(
        &self,
        store: &TaskStore,
        subtree_root_id: &TaskId,
    ) -> Result<Vec<TaskId>, String> {
        self.inner.remove_subtree(store, subtree_root_id)
    }
}

impl LlmGraphReflector {
    /// 使用 LLM 分析当前子树并决定是否需要重规划。
    /// 如果返回 replace，自动移除旧子树并插入新子树。
    pub fn reflect_and_replan(
        &self,
        store: &TaskStore,
        parent_task_id: &TaskId,
    ) -> Result<ReplanDecision, String> {
        let prompt = Self::build_replan_prompt(store, parent_task_id)
            .ok_or_else(|| format!("parent {} not found", parent_task_id))?;

        let parent = store
            .get_task(parent_task_id)
            .ok_or_else(|| format!("parent {} not found", parent_task_id))?;

        let request = ModelInvocationRequest {
            provider: "openai-compat".to_string(),
            prompt: String::new(),
            messages: Some(vec![ChatMessage {
                role: "user".to_string(),
                content: Some(prompt),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }]),
            tools: None,
            tool_choice: None,
        };

        let response = self
            .client
            .invoke(request)
            .map_err(|e| format!("LLM 调用失败: {e}"))?;
        if !response.ok {
            return Err(format!("LLM 返回错误: {}", response.payload));
        }

        let payload = response.parse_chat_payload();
        let text = payload.content.unwrap_or_default();
        let json_text = text
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        let value: serde_json::Value =
            serde_json::from_str(json_text).map_err(|e| format!("JSON 解析失败: {e}"))?;

        let action = value["action"].as_str().unwrap_or("keep");
        let reason = value["reason"].as_str().unwrap_or("no reason").to_string();

        if action == "keep" {
            return Ok(ReplanDecision {
                action: ReplanAction::Keep,
                reason,
                added_tasks: Vec::new(),
                removed_tasks: Vec::new(),
            });
        }

        let children = store.get_children(parent_task_id);
        let mut removed_tasks = Vec::new();
        for child in &children {
            if !is_terminal(child.status) {
                let removed = self.inner.remove_subtree(store, &child.task_id)?;
                removed_tasks.extend(removed);
            }
        }

        let new_task_items = value["new_tasks"].as_array().cloned().unwrap_or_default();
        let mut added_tasks = Vec::new();
        for item in &new_task_items {
            let id_suffix = item["id"].as_str().unwrap_or("unknown");
            let task_id = TaskId::new(format!("{}-replan-{}", parent_task_id, id_suffix));
            let kind = match item["kind"].as_str().unwrap_or("Action") {
                "Phase" => TaskKind::Phase,
                "WorkPackage" => TaskKind::WorkPackage,
                "Validation" => TaskKind::Validation,
                _ => TaskKind::Action,
            };
            let task = Task {
                task_id: task_id.clone(),
                mission_id: parent.mission_id.clone(),
                root_task_id: parent.root_task_id.clone(),
                parent_task_id: Some(parent_task_id.clone()),
                kind,
                title: item["title"].as_str().unwrap_or("Untitled").to_string(),
                goal: item["goal"].as_str().unwrap_or("").to_string(),
                status: TaskStatus::Draft,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: parent.policy_snapshot.clone(),
                executor_binding: None,
                context_refs: Vec::new(),
                knowledge_refs: Vec::new(),
                workspace_scope: parent.workspace_scope.clone(),
                write_scope: item["write_scope"].as_str().map(|s| s.to_string()),
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                repair_count: 0,
                decision_payload: None,
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            };
            store.insert_task(task);
            added_tasks.push(task_id);
        }

        Ok(ReplanDecision {
            action: ReplanAction::Replace,
            reason,
            added_tasks,
            removed_tasks,
        })
    }
}

#[derive(Clone, Debug)]
pub enum ReplanAction {
    Keep,
    Replace,
}

#[derive(Clone, Debug)]
pub struct ReplanDecision {
    pub action: ReplanAction,
    pub reason: String,
    pub added_tasks: Vec<TaskId>,
    pub removed_tasks: Vec<TaskId>,
}

// ---------------------------------------------------------------------------
// G11: Mission decomposition trait (design 4.2.1)
// ---------------------------------------------------------------------------

/// Trait for decomposing a mission into a task graph.
///
/// Implementations typically use an LLM to analyze the user request and
/// generate a structured task graph.
pub trait MissionDecomposer: Send + Sync {
    /// Analyze the mission goal and produce a set of tasks forming the
    /// execution graph. The returned tasks should have `parent_task_id`
    /// and `dependency_ids` set correctly.
    fn decompose(
        &self,
        mission_id: &magi_core::MissionId,
        root_task_id: &TaskId,
        goal: &str,
    ) -> Result<Vec<Task>, String>;
}

// ---------------------------------------------------------------------------
// G11.1: LLM-based MissionDecomposer (design 4.2.1)
// ---------------------------------------------------------------------------

pub struct LlmMissionDecomposer {
    client: Arc<dyn ModelBridgeClient>,
}

impl LlmMissionDecomposer {
    pub fn new(client: Arc<dyn ModelBridgeClient>) -> Self {
        Self { client }
    }

    fn build_decomposition_prompt(goal: &str) -> String {
        format!(
            r#"你是一个任务分解器。请将以下目标分解为可执行的任务图。

目标: {goal}

请以 JSON 数组形式返回任务列表，每个任务包含:
- "id": 任务编号（如 "phase-1", "wp-1-1", "act-1-1-1"）
- "kind": 任务类型（"Phase" / "WorkPackage" / "Action" / "Validation"）
- "parent_id": 父任务编号（根节点留空）
- "title": 任务标题
- "goal": 任务目标描述
- "dependency_ids": 依赖的任务编号数组
- "write_scope": 文件写入范围（可选）

只返回 JSON 数组，不要其他内容。"#
        )
    }

    fn parse_decomposition_response(
        response_text: &str,
        mission_id: &magi_core::MissionId,
        root_task_id: &TaskId,
    ) -> Result<Vec<Task>, String> {
        let json_text = response_text
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        let items: Vec<serde_json::Value> =
            serde_json::from_str(json_text).map_err(|e| format!("JSON 解析失败: {e}"))?;

        if items.is_empty() {
            return Err("LLM 返回了空任务列表".to_string());
        }

        let mut tasks = Vec::new();
        for item in &items {
            let id_suffix = item["id"].as_str().unwrap_or("unknown");
            let task_id = TaskId::new(format!("{}-{}", root_task_id, id_suffix));
            let kind = match item["kind"].as_str().unwrap_or("Action") {
                "Phase" => TaskKind::Phase,
                "WorkPackage" => TaskKind::WorkPackage,
                "Validation" => TaskKind::Validation,
                _ => TaskKind::Action,
            };
            let parent_task_id = item["parent_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|pid| TaskId::new(format!("{}-{}", root_task_id, pid)))
                .or_else(|| Some(root_task_id.clone()));
            let dependency_ids: Vec<TaskId> = item["dependency_ids"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|did| TaskId::new(format!("{}-{}", root_task_id, did)))
                        .collect()
                })
                .unwrap_or_default();

            tasks.push(Task {
                task_id,
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                parent_task_id,
                kind,
                title: item["title"].as_str().unwrap_or("Untitled").to_string(),
                goal: item["goal"].as_str().unwrap_or("").to_string(),
                status: TaskStatus::Draft,
                dependency_ids,
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                context_refs: Vec::new(),
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: item["write_scope"].as_str().map(|s| s.to_string()),
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                repair_count: 0,
                decision_payload: None,
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            });
        }

        Ok(tasks)
    }
}

impl MissionDecomposer for LlmMissionDecomposer {
    fn decompose(
        &self,
        mission_id: &magi_core::MissionId,
        root_task_id: &TaskId,
        goal: &str,
    ) -> Result<Vec<Task>, String> {
        let prompt = Self::build_decomposition_prompt(goal);
        let request = ModelInvocationRequest {
            provider: "openai-compat".to_string(),
            prompt: String::new(),
            messages: Some(vec![ChatMessage {
                role: "user".to_string(),
                content: Some(prompt),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }]),
            tools: None,
            tool_choice: None,
        };
        let response = self
            .client
            .invoke(request)
            .map_err(|e| format!("LLM 调用失败: {e}"))?;
        if !response.ok {
            return Err(format!("LLM 返回错误: {}", response.payload));
        }
        let payload = response.parse_chat_payload();
        let text = payload.content.unwrap_or_default();
        Self::parse_decomposition_response(&text, mission_id, root_task_id)
    }
}

/// A task status is terminal when no further transitions are expected.
fn is_terminal(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Skipped
    )
}

/// Two write scopes conflict if either is a prefix of the other
/// (path-based containment), or they are identical.
fn scopes_conflict(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let a_slash = if a.ends_with('/') {
        a.to_string()
    } else {
        format!("{a}/")
    };
    let b_slash = if b.ends_with('/') {
        b.to_string()
    } else {
        format!("{b}/")
    };
    b_slash.starts_with(&a_slash) || a_slash.starts_with(&b_slash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{
        AssignmentLease, LeaseId, LeaseStatus, MissionId, Task, TaskId, TaskKind, TaskStatus,
        UtcMillis, WorkerId,
    };

    fn make_task(
        task_id: &str,
        mission_id: &str,
        root_task_id: &str,
        parent_task_id: Option<&str>,
        kind: TaskKind,
        status: TaskStatus,
    ) -> Task {
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new(mission_id),
            root_task_id: TaskId::new(root_task_id),
            parent_task_id: parent_task_id.map(TaskId::new),
            kind,
            title: format!("Task {task_id}"),
            goal: format!("Goal for {task_id}"),
            status,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        }
    }

    fn make_worker(id: &str, role: &str, kinds: Vec<TaskKind>) -> WorkerInfo {
        WorkerInfo {
            worker_id: WorkerId::new(id),
            role: role.to_string(),
            supported_kinds: kinds,
            parallelism_limit: None,
        }
    }

    // -----------------------------------------------------------------------
    // Test 1: Single task dispatch cycle
    // -----------------------------------------------------------------------

    #[test]
    fn single_task_dispatch_cycle() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let action = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );

        store.insert_task(root);
        store.insert_task(action);

        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::new(Arc::clone(&store), workers);

        // First cycle: should dispatch act-1.
        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::Continue);

        // act-1 should now be Running with an active lease.
        let act = store.get_task(&TaskId::new("act-1")).unwrap();
        assert_eq!(act.status, TaskStatus::Running);
        assert!(store.get_active_lease(&TaskId::new("act-1")).is_some());
    }

    // -----------------------------------------------------------------------
    // Test 2: Multi-task parallel dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn multi_task_parallel_dispatch() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        let act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );

        store.insert_task(root);
        store.insert_task(act1);
        store.insert_task(act2);

        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::new(Arc::clone(&store), workers);

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::Continue);

        // Both actions should be Running.
        let a1 = store.get_task(&TaskId::new("act-1")).unwrap();
        let a2 = store.get_task(&TaskId::new("act-2")).unwrap();
        assert_eq!(a1.status, TaskStatus::Running);
        assert_eq!(a2.status, TaskStatus::Running);

        // Both should have active leases.
        assert!(store.get_active_lease(&TaskId::new("act-1")).is_some());
        assert!(store.get_active_lease(&TaskId::new("act-2")).is_some());
    }

    // -----------------------------------------------------------------------
    // Test 3: Lease expiry handling
    // -----------------------------------------------------------------------

    #[test]
    fn lease_expiry_resets_task_to_ready() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let action = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Running,
        );

        store.insert_task(root);
        store.insert_task(action);

        // Insert an already-expired lease.
        let now = UtcMillis::now();
        let expired_lease = AssignmentLease {
            lease_id: LeaseId::new("lease-expired"),
            task_id: TaskId::new("act-1"),
            worker_id: WorkerId::new("w-1"),
            role: "executor".to_string(),
            granted_at: UtcMillis(now.0.saturating_sub(120_000)),
            expires_at: UtcMillis(now.0.saturating_sub(60_000)),
            heartbeat_at: UtcMillis(now.0.saturating_sub(120_000)),
            lease_status: LeaseStatus::Active,
        };
        store.insert_lease(expired_lease);

        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::new(Arc::clone(&store), workers);

        // The cycle should revoke the expired lease and reset act-1 to Ready,
        // then re-dispatch it.
        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::Continue);

        // act-1 should be Running again with a new lease.
        let act = store.get_task(&TaskId::new("act-1")).unwrap();
        assert_eq!(act.status, TaskStatus::Running);

        // The old expired lease should be revoked.
        let old_lease_active = store.get_active_lease(&TaskId::new("act-1"));
        assert!(old_lease_active.is_some());
        // The new lease should NOT be the expired one.
        assert_ne!(
            old_lease_active.unwrap().lease_id.to_string(),
            "lease-expired"
        );
    }

    // -----------------------------------------------------------------------
    // Test 4: Parent completion when all children complete
    // -----------------------------------------------------------------------

    #[test]
    fn parent_completed_when_all_children_complete() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        );
        let act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        );

        store.insert_task(root);
        store.insert_task(act1);
        store.insert_task(act2);

        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::new(Arc::clone(&store), workers);

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::AllComplete);

        // The root objective should have been propagated to Completed.
        let obj = store.get_task(&TaskId::new("obj-1")).unwrap();
        assert_eq!(obj.status, TaskStatus::Completed);
    }

    // -----------------------------------------------------------------------
    // Test 5: Parent fails when any child fails
    // -----------------------------------------------------------------------

    #[test]
    fn parent_fails_when_any_child_fails() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        );
        let act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Failed,
        );

        store.insert_task(root);
        store.insert_task(act1);
        store.insert_task(act2);

        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::new(Arc::clone(&store), workers);

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::AllComplete);

        let obj = store.get_task(&TaskId::new("obj-1")).unwrap();
        assert_eq!(obj.status, TaskStatus::Failed);
    }

    // -----------------------------------------------------------------------
    // Test 6: Blocked state when no workers match
    // -----------------------------------------------------------------------

    #[test]
    fn blocked_when_no_workers_match() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let val = make_task(
            "val-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Validation,
            TaskStatus::Ready,
        );

        store.insert_task(root);
        store.insert_task(val);

        // Worker only supports Action, not Validation.
        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::new(Arc::clone(&store), workers);

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        match outcome {
            RunCycleOutcome::Blocked(ids) => {
                assert!(ids.iter().any(|id| id.to_string() == "val-1"));
            }
            other => panic!("expected Blocked, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Test 7: No workers at all produces Blocked
    // -----------------------------------------------------------------------

    #[test]
    fn no_workers_produces_blocked() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );

        store.insert_task(root);
        store.insert_task(act);

        let runner = TaskRunner::new(Arc::clone(&store), Vec::new());

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        match outcome {
            RunCycleOutcome::Blocked(ids) => {
                assert!(ids.iter().any(|id| id.to_string() == "act-1"));
            }
            other => panic!("expected Blocked, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Test 8: Decision tasks are not dispatched to workers
    // -----------------------------------------------------------------------

    #[test]
    fn decision_tasks_are_not_dispatched() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let decision = make_task(
            "dec-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Decision,
            TaskStatus::Ready,
        );

        store.insert_task(root);
        store.insert_task(decision);

        // Even though the worker claims to support Decision, it should not be dispatched.
        let workers = vec![make_worker("w-1", "architect", vec![TaskKind::Decision])];
        let runner = TaskRunner::new(Arc::clone(&store), workers);

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        // Decision is skipped, nothing was dispatched, and there are non-terminal tasks.
        match outcome {
            RunCycleOutcome::Blocked(_) => {} // expected
            other => panic!("expected Blocked, got {:?}", other),
        }

        // Decision task should still be Ready (not Running).
        let dec = store.get_task(&TaskId::new("dec-1")).unwrap();
        assert_eq!(dec.status, TaskStatus::Ready);
        assert!(store.get_active_lease(&TaskId::new("dec-1")).is_none());
    }

    // -----------------------------------------------------------------------
    // Test 9: Multi-level tree propagation
    // -----------------------------------------------------------------------

    #[test]
    fn multi_level_tree_propagation() {
        let store = Arc::new(TaskStore::new());

        // Objective -> Phase -> 2 completed Actions
        let obj = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let phase = make_task(
            "phase-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Phase,
            TaskStatus::Running,
        );
        let act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("phase-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        );
        let act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("phase-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        );

        store.insert_task(obj);
        store.insert_task(phase);
        store.insert_task(act1);
        store.insert_task(act2);

        let runner = TaskRunner::new(Arc::clone(&store), Vec::new());

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::AllComplete);

        // Phase should be Completed.
        let p = store.get_task(&TaskId::new("phase-1")).unwrap();
        assert_eq!(p.status, TaskStatus::Completed);

        // Objective should be Completed.
        let o = store.get_task(&TaskId::new("obj-1")).unwrap();
        assert_eq!(o.status, TaskStatus::Completed);
    }

    // -----------------------------------------------------------------------
    // Test 10: Validation tasks dispatched to matching worker
    // -----------------------------------------------------------------------

    #[test]
    fn validation_task_dispatched_to_validator_worker() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let val = make_task(
            "val-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Validation,
            TaskStatus::Ready,
        );

        store.insert_task(root);
        store.insert_task(val);

        let workers = vec![
            make_worker("w-exec", "integration-dev", vec![TaskKind::Action]),
            make_worker("w-val", "reviewer", vec![TaskKind::Validation]),
        ];
        let runner = TaskRunner::new(Arc::clone(&store), workers);

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::Continue);

        let v = store.get_task(&TaskId::new("val-1")).unwrap();
        assert_eq!(v.status, TaskStatus::Running);

        // Lease should be assigned to w-val.
        let lease = store.get_active_lease(&TaskId::new("val-1")).unwrap();
        assert_eq!(lease.worker_id.to_string(), "w-val");
    }

    // -----------------------------------------------------------------------
    // Test 11: Dispatcher callback is invoked on dispatch
    // -----------------------------------------------------------------------

    /// A recording dispatcher that logs every dispatch call for later assertion.
    struct RecordingDispatcher {
        dispatches: std::sync::Mutex<Vec<(String, String, String)>>,
    }

    impl RecordingDispatcher {
        fn new() -> Self {
            Self {
                dispatches: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn dispatches(&self) -> Vec<(String, String, String)> {
            self.dispatches.lock().unwrap().clone()
        }
    }

    impl TaskDispatcher for RecordingDispatcher {
        fn dispatch(
            &self,
            task: &Task,
            worker: &WorkerInfo,
            lease: &AssignmentLease,
        ) -> Result<(), String> {
            self.dispatches.lock().unwrap().push((
                task.task_id.to_string(),
                worker.worker_id.to_string(),
                lease.lease_id.to_string(),
            ));
            Ok(())
        }
    }

    #[test]
    fn dispatcher_callback_is_invoked_on_dispatch() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        let act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );

        store.insert_task(root);
        store.insert_task(act1);
        store.insert_task(act2);

        let dispatcher = Arc::new(RecordingDispatcher::new());
        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            workers,
            Arc::clone(&dispatcher) as Arc<dyn TaskDispatcher>,
            Arc::new(NoOpResultReceiver),
        );

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::Continue);

        // The dispatcher should have been called once per dispatched task.
        let records = dispatcher.dispatches();
        assert_eq!(records.len(), 2);

        let task_ids: Vec<&str> = records.iter().map(|(tid, _, _)| tid.as_str()).collect();
        assert!(task_ids.contains(&"act-1"));
        assert!(task_ids.contains(&"act-2"));

        // All dispatches should have been assigned to w-1.
        for (_, wid, _) in &records {
            assert_eq!(wid, "w-1");
        }
    }

    // -----------------------------------------------------------------------
    // Test 12: Result receiver feeds back into the cycle
    // -----------------------------------------------------------------------

    /// A result receiver that returns a fixed set of results exactly once.
    struct FixedResultReceiver {
        results: std::sync::Mutex<Vec<TaskResult>>,
    }

    impl FixedResultReceiver {
        fn new(results: Vec<TaskResult>) -> Self {
            Self {
                results: std::sync::Mutex::new(results),
            }
        }
    }

    impl TaskResultReceiver for FixedResultReceiver {
        fn poll_results(&self) -> Vec<TaskResult> {
            let mut guard = self.results.lock().unwrap();
            std::mem::take(&mut *guard)
        }
    }

    #[test]
    fn result_receiver_applies_completed_outcome() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Running,
        );

        store.insert_task(root);
        store.insert_task(act);

        // Insert a lease for the running task.
        let now = UtcMillis::now();
        let lease = AssignmentLease {
            lease_id: LeaseId::new("lease-act-1"),
            task_id: TaskId::new("act-1"),
            worker_id: WorkerId::new("w-1"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0 + 120_000),
            heartbeat_at: now,
            lease_status: LeaseStatus::Active,
        };
        store.insert_lease(lease);

        let receiver = Arc::new(FixedResultReceiver::new(vec![TaskResult {
            task_id: TaskId::new("act-1"),
            lease_id: LeaseId::new("lease-act-1"),
            outcome: TaskOutcome::Completed {
                output_refs: vec!["output-1".to_string()],
            },
        }]));

        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(NoOpDispatcher),
            receiver,
        );

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::AllComplete);

        // act-1 should be Completed and root should have propagated.
        let act = store.get_task(&TaskId::new("act-1")).unwrap();
        assert_eq!(act.status, TaskStatus::Completed);

        let obj = store.get_task(&TaskId::new("obj-1")).unwrap();
        assert_eq!(obj.status, TaskStatus::Completed);
    }

    #[test]
    fn result_receiver_ignores_completed_outcome_when_lease_was_revoked() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Blocked,
        );

        store.insert_task(root);
        store.insert_task(act);

        let now = UtcMillis::now();
        let lease = AssignmentLease {
            lease_id: LeaseId::new("lease-act-1"),
            task_id: TaskId::new("act-1"),
            worker_id: WorkerId::new("w-1"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0 + 120_000),
            heartbeat_at: now,
            lease_status: LeaseStatus::Active,
        };
        store.insert_lease(lease);
        assert!(
            store.revoke_lease(&TaskId::new("act-1"), &LeaseId::new("lease-act-1")),
            "lease should be revoked before stale result arrives"
        );

        let receiver = Arc::new(FixedResultReceiver::new(vec![TaskResult {
            task_id: TaskId::new("act-1"),
            lease_id: LeaseId::new("lease-act-1"),
            outcome: TaskOutcome::Completed {
                output_refs: vec!["late-output".to_string()],
            },
        }]));

        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(NoOpDispatcher),
            receiver,
        );

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_ne!(outcome, RunCycleOutcome::AllComplete);

        let act = store.get_task(&TaskId::new("act-1")).unwrap();
        assert_eq!(act.status, TaskStatus::Blocked);
        assert!(
            act.output_refs.is_empty(),
            "stale result must not write output refs"
        );

        let obj = store.get_task(&TaskId::new("obj-1")).unwrap();
        assert_eq!(obj.status, TaskStatus::Blocked);
    }

    // -----------------------------------------------------------------------
    // Test 13: Failed result marks task as Failed
    // -----------------------------------------------------------------------

    #[test]
    fn result_receiver_applies_failed_outcome() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Running,
        );

        store.insert_task(root);
        store.insert_task(act);

        let now = UtcMillis::now();
        let lease = AssignmentLease {
            lease_id: LeaseId::new("lease-act-1"),
            task_id: TaskId::new("act-1"),
            worker_id: WorkerId::new("w-1"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0 + 120_000),
            heartbeat_at: now,
            lease_status: LeaseStatus::Active,
        };
        store.insert_lease(lease);

        let receiver = Arc::new(FixedResultReceiver::new(vec![TaskResult {
            task_id: TaskId::new("act-1"),
            lease_id: LeaseId::new("lease-act-1"),
            outcome: TaskOutcome::Failed {
                error: "compilation error".to_string(),
            },
        }]));

        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(NoOpDispatcher),
            receiver,
        );

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::AllComplete);

        let act = store.get_task(&TaskId::new("act-1")).unwrap();
        assert_eq!(act.status, TaskStatus::Failed);

        let obj = store.get_task(&TaskId::new("obj-1")).unwrap();
        assert_eq!(obj.status, TaskStatus::Failed);
    }

    // -----------------------------------------------------------------------
    // Test 14: Dispatcher error propagates as RunCycleOutcome::Error
    // -----------------------------------------------------------------------

    struct FailingDispatcher;

    impl TaskDispatcher for FailingDispatcher {
        fn dispatch(
            &self,
            _task: &Task,
            _worker: &WorkerInfo,
            _lease: &AssignmentLease,
        ) -> Result<(), String> {
            Err("worker unavailable".to_string())
        }
    }

    #[test]
    fn dispatcher_error_propagates_as_cycle_error() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );

        store.insert_task(root);
        store.insert_task(act);

        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            workers,
            Arc::new(FailingDispatcher),
            Arc::new(NoOpResultReceiver),
        );

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        match outcome {
            RunCycleOutcome::Error(msg) => {
                assert!(msg.contains("dispatcher failed"));
                assert!(msg.contains("worker unavailable"));
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Test 15: Write scope conflict blocks parallel dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn write_scope_conflict_blocks_parallel_dispatch() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let mut act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        act1.write_scope = Some("src/components".to_string());
        let mut act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        act2.write_scope = Some("src/components/button".to_string());

        store.insert_task(root);
        store.insert_task(act1);
        store.insert_task(act2);

        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::new(Arc::clone(&store), workers);

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::Continue);

        // Only one should be Running (the first one dispatched blocks the second).
        let a1 = store.get_task(&TaskId::new("act-1")).unwrap();
        let a2 = store.get_task(&TaskId::new("act-2")).unwrap();
        let running_count = [a1.status, a2.status]
            .iter()
            .filter(|s| **s == TaskStatus::Running)
            .count();
        assert_eq!(
            running_count, 1,
            "only one task should be running due to write scope conflict"
        );
    }

    // -----------------------------------------------------------------------
    // Test 16: scopes_conflict function
    // -----------------------------------------------------------------------

    #[test]
    fn scopes_conflict_detects_containment() {
        assert!(scopes_conflict("src", "src"));
        assert!(scopes_conflict("src", "src/foo"));
        assert!(scopes_conflict("src/foo", "src"));
        assert!(scopes_conflict("src/foo", "src/foo/bar"));
        assert!(!scopes_conflict("src/foo", "src/bar"));
        assert!(!scopes_conflict("src/foo", "lib/foo"));
    }

    // -----------------------------------------------------------------------
    // Test 17: Exclusive scope conflict blocks dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn exclusive_scope_conflict_blocks_dispatch() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let mut act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        act1.executor_binding = Some(magi_core::ExecutorBinding {
            target_role: "integration-dev".to_string(),
            capability_requirements: Vec::new(),
            parallelism_group: None,
            exclusive_scope: Some("deploy-prod".to_string()),
            worker_selector: None,
        });
        let mut act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        act2.executor_binding = Some(magi_core::ExecutorBinding {
            target_role: "integration-dev".to_string(),
            capability_requirements: Vec::new(),
            parallelism_group: None,
            exclusive_scope: Some("deploy-prod".to_string()),
            worker_selector: None,
        });

        store.insert_task(root);
        store.insert_task(act1);
        store.insert_task(act2);

        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::new(Arc::clone(&store), workers);

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::Continue);

        let a1 = store.get_task(&TaskId::new("act-1")).unwrap();
        let a2 = store.get_task(&TaskId::new("act-2")).unwrap();
        let running_count = [a1.status, a2.status]
            .iter()
            .filter(|s| **s == TaskStatus::Running)
            .count();
        assert_eq!(
            running_count, 1,
            "exclusive scope should prevent parallel dispatch"
        );
    }

    // -----------------------------------------------------------------------
    // Test 18: Blocked child propagates to parent
    // -----------------------------------------------------------------------

    #[test]
    fn blocked_child_propagates_to_parent() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        );
        let act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Blocked,
        );

        store.insert_task(root);
        store.insert_task(act1);
        store.insert_task(act2);

        let runner = TaskRunner::new(Arc::clone(&store), Vec::new());
        let _outcome = runner.run_cycle(&TaskId::new("obj-1"));

        let obj = store.get_task(&TaskId::new("obj-1")).unwrap();
        assert_eq!(
            obj.status,
            TaskStatus::Blocked,
            "parent should be Blocked when any required child is Blocked"
        );
    }

    // -----------------------------------------------------------------------
    // Test 19: required_children controls parent aggregation
    // -----------------------------------------------------------------------

    #[test]
    fn required_children_controls_aggregation() {
        let store = Arc::new(TaskStore::new());

        let mut root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        root.required_children = vec![TaskId::new("act-1")];
        let act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        );
        let act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Failed,
        );

        store.insert_task(root);
        store.insert_task(act1);
        store.insert_task(act2);

        let runner = TaskRunner::new(Arc::clone(&store), Vec::new());
        let _outcome = runner.run_cycle(&TaskId::new("obj-1"));

        let obj = store.get_task(&TaskId::new("obj-1")).unwrap();
        assert_eq!(
            obj.status,
            TaskStatus::Completed,
            "parent should complete because only required child (act-1) is Completed; act-2 (Failed) is not required"
        );
    }

    // -----------------------------------------------------------------------
    // Test 20: NeedsRepair creates Repair child task
    // -----------------------------------------------------------------------

    #[test]
    fn needs_repair_creates_repair_child() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Running,
        );

        store.insert_task(root);
        store.insert_task(act);

        let now = UtcMillis::now();
        let lease = AssignmentLease {
            lease_id: LeaseId::new("lease-act-1"),
            task_id: TaskId::new("act-1"),
            worker_id: WorkerId::new("w-1"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0 + 120_000),
            heartbeat_at: now,
            lease_status: LeaseStatus::Active,
        };
        store.insert_lease(lease);

        let receiver = Arc::new(FixedResultReceiver::new(vec![TaskResult {
            task_id: TaskId::new("act-1"),
            lease_id: LeaseId::new("lease-act-1"),
            outcome: TaskOutcome::NeedsRepair {
                reason: "test failure".to_string(),
            },
        }]));

        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(NoOpDispatcher),
            receiver,
        );

        let _outcome = runner.run_cycle(&TaskId::new("obj-1"));

        // act-1 should be Repairing.
        let act = store.get_task(&TaskId::new("act-1")).unwrap();
        assert_eq!(act.status, TaskStatus::Repairing);
        assert_eq!(act.repair_count, 1);

        // A Repair child task should have been created.
        let children = store.get_children(&TaskId::new("act-1"));
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].kind, TaskKind::Repair);
        assert_eq!(children[0].status, TaskStatus::Ready);
        assert!(children[0].goal.contains("test failure"));
    }

    // -----------------------------------------------------------------------
    // Test 21: Repair budget exhaustion marks as Failed
    // -----------------------------------------------------------------------

    #[test]
    fn repair_budget_exhausted_marks_failed() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let mut act = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Running,
        );
        act.repair_count = 3; // Already at default limit
        act.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "auto".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 3,
            repair_limit: 3,
            validation_profile: None,
            checkpoint_mode: "auto".to_string(),
            background_allowed: false,
            escalation_conditions: Vec::new(),
        });

        store.insert_task(root);
        store.insert_task(act);

        let now = UtcMillis::now();
        let lease = AssignmentLease {
            lease_id: LeaseId::new("lease-act-1"),
            task_id: TaskId::new("act-1"),
            worker_id: WorkerId::new("w-1"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0 + 120_000),
            heartbeat_at: now,
            lease_status: LeaseStatus::Active,
        };
        store.insert_lease(lease);

        let receiver = Arc::new(FixedResultReceiver::new(vec![TaskResult {
            task_id: TaskId::new("act-1"),
            lease_id: LeaseId::new("lease-act-1"),
            outcome: TaskOutcome::NeedsRepair {
                reason: "still broken".to_string(),
            },
        }]));

        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(NoOpDispatcher),
            receiver,
        );

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::AllComplete);

        let act = store.get_task(&TaskId::new("act-1")).unwrap();
        assert_eq!(
            act.status,
            TaskStatus::Failed,
            "should fail when repair budget exhausted"
        );

        // No Repair child should have been created.
        let children = store.get_children(&TaskId::new("act-1"));
        assert!(children.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test 22: Manual autonomy_level blocks dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn manual_autonomy_blocks_dispatch() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let mut act = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        act.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Manual".to_string(),
            approval_mode: "explicit".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 3,
            repair_limit: 3,
            validation_profile: None,
            checkpoint_mode: "auto".to_string(),
            background_allowed: false,
            escalation_conditions: Vec::new(),
        });

        store.insert_task(root);
        store.insert_task(act);

        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::new(Arc::clone(&store), workers);

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        match outcome {
            RunCycleOutcome::Blocked(_) => {}
            other => panic!("expected Blocked (Manual policy), got {:?}", other),
        }

        let act = store.get_task(&TaskId::new("act-1")).unwrap();
        assert_eq!(
            act.status,
            TaskStatus::Ready,
            "Manual task should remain Ready"
        );
    }

    #[test]
    fn pause_and_resume_task_propagate_recursively_to_worker_branches() {
        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let phase = make_task(
            "phase-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Phase,
            TaskStatus::Running,
        );
        let wp = make_task(
            "wp-1",
            "m-1",
            "obj-1",
            Some("phase-1"),
            TaskKind::WorkPackage,
            TaskStatus::Running,
        );
        let act = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("wp-1"),
            TaskKind::Action,
            TaskStatus::Running,
        );

        store.insert_task(root);
        store.insert_task(phase);
        store.insert_task(wp);
        store.insert_task(act);

        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::new(Arc::clone(&store), workers);

        runner
            .pause_task(&TaskId::new("obj-1"))
            .expect("pause should block whole subtree");
        for task_id in ["obj-1", "phase-1", "wp-1", "act-1"] {
            let task = store
                .get_task(&TaskId::new(task_id))
                .expect("task should exist after pause");
            assert_eq!(
                task.status,
                TaskStatus::Blocked,
                "{task_id} should become Blocked"
            );
        }

        runner
            .resume_task(&TaskId::new("obj-1"))
            .expect("resume should reopen whole subtree");
        assert_eq!(
            store.get_task(&TaskId::new("obj-1")).unwrap().status,
            TaskStatus::Running
        );
        for task_id in ["phase-1", "wp-1", "act-1"] {
            let task = store
                .get_task(&TaskId::new(task_id))
                .expect("task should exist after resume");
            assert_eq!(
                task.status,
                TaskStatus::Ready,
                "{task_id} should become Ready"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test 23: WorkerExecutionDispatcher completes full dispatch+execute loop
    // -----------------------------------------------------------------------

    #[test]
    fn worker_execution_dispatcher_full_loop() {
        use magi_event_bus::InMemoryEventBus;

        let event_bus = Arc::new(InMemoryEventBus::new(256));
        let worker_runtime = magi_worker_runtime::WorkerRuntime::new(Arc::clone(&event_bus));
        let result_receiver = Arc::new(EventBasedResultReceiver::new());

        let dispatcher =
            WorkerExecutionDispatcher::new(worker_runtime, Arc::clone(&result_receiver))
                .with_event_bus(Arc::clone(&event_bus));

        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );

        store.insert_task(root);
        store.insert_task(act);

        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            workers,
            Arc::new(dispatcher),
            Arc::clone(&result_receiver) as Arc<dyn TaskResultReceiver>,
        );

        // Cycle 1: dispatch + execute act-1; result is buffered.
        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::Continue);

        let act = store.get_task(&TaskId::new("act-1")).unwrap();
        assert_eq!(act.status, TaskStatus::Running);

        // Cycle 2: apply buffered result → act-1 completes → parent propagates.
        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::AllComplete);

        let act = store.get_task(&TaskId::new("act-1")).unwrap();
        assert!(
            act.status == TaskStatus::Completed || act.status == TaskStatus::Failed,
            "act-1 should reach a terminal state, got {:?}",
            act.status
        );

        let obj = store.get_task(&TaskId::new("obj-1")).unwrap();
        assert!(
            obj.status == TaskStatus::Completed || obj.status == TaskStatus::Failed,
            "obj-1 should propagate to terminal, got {:?}",
            obj.status
        );
    }

    // -----------------------------------------------------------------------
    // Test 24: WorkerExecutionDispatcher multi-task sequential execution
    // -----------------------------------------------------------------------

    #[test]
    fn worker_execution_dispatcher_multi_task() {
        use magi_event_bus::InMemoryEventBus;

        let event_bus = Arc::new(InMemoryEventBus::new(256));
        let worker_runtime = magi_worker_runtime::WorkerRuntime::new(Arc::clone(&event_bus));
        let result_receiver = Arc::new(EventBasedResultReceiver::new());

        let dispatcher =
            WorkerExecutionDispatcher::new(worker_runtime, Arc::clone(&result_receiver));

        let store = Arc::new(TaskStore::new());

        let root = make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        let act1 = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        let act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );

        store.insert_task(root);
        store.insert_task(act1);
        store.insert_task(act2);

        let workers = vec![make_worker(
            "w-1",
            "integration-dev",
            vec![TaskKind::Action],
        )];
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            workers,
            Arc::new(dispatcher),
            Arc::clone(&result_receiver) as Arc<dyn TaskResultReceiver>,
        );

        // Cycle 1: dispatches both tasks.
        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::Continue);

        // Both should be Running after dispatch.
        assert_eq!(
            store.get_task(&TaskId::new("act-1")).unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(
            store.get_task(&TaskId::new("act-2")).unwrap().status,
            TaskStatus::Running
        );

        // Cycle 2: apply results from both tasks.
        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::AllComplete);

        // Parent should propagate to a terminal state.
        let obj = store.get_task(&TaskId::new("obj-1")).unwrap();
        assert!(
            is_terminal(obj.status),
            "obj-1 should be terminal after all children complete, got {:?}",
            obj.status
        );
    }
}
