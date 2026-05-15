//! Task System v2 — M18：TaskRunner 调度循环从 magi-orchestrator::task_runner
//! 下沉到 conversation-runtime。
//!
//! magi-orchestrator 暂时保留 WorkerInfo / TaskDispatcher / TaskOutcome /
//! EventBased* / WorkerExecutionDispatcher 等执行桥类型；TaskRunner 本体只依赖这些
//! trait 与 TaskStore，因此可先迁到 v2 runtime，后续 M19 再拆剩余桥层。

use crate::task_runner_bridge::{
    NoOpDispatcher, NoOpResultReceiver, RunCycleOutcome, TaskDispatcher, TaskOutcome,
    TaskResultReceiver,
};
#[cfg(test)]
use crate::task_runner_bridge::{EventBasedResultReceiver, TaskResult, WorkerExecutionDispatcher};
use magi_agent_role::AgentRoleRegistry;
use magi_core::{EventId, Task, TaskId, TaskKind, TaskStatus, UtcMillis};
use magi_event_bus::{EventEnvelope, InMemoryEventBus};
use magi_orchestrator::task_store::TaskStore;
use magi_orchestrator::task_worker_catalog::{WorkerInfo, resolve_task_role};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const DEFAULT_LEASE_DURATION_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum DispatchPolicyOutcome {
    Allow,
    Reject(String),
    NeedsApproval(String),
}

fn decision_payload_text(payload: &serde_json::Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

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
    /// Optional event bus for publishing domain events such as DecisionCreated.
    event_bus: Option<Arc<InMemoryEventBus>>,
    /// Checkpoint 信号：当 cycle 中有任务到达终态时置为 true，供外部消费。
    checkpoint_signal: AtomicBool,
    /// Task System v2：解析 task → role 的角色注册表。
    agent_role_registry: AgentRoleRegistry,
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
            event_bus: None,
            checkpoint_signal: AtomicBool::new(false),
            agent_role_registry: AgentRoleRegistry::load_default(),
        }
    }

    /// Override the registry (tests + 自定义 catalog 用)。
    pub fn with_agent_role_registry(mut self, registry: AgentRoleRegistry) -> Self {
        self.agent_role_registry = registry;
        self
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
            event_bus: None,
            checkpoint_signal: AtomicBool::new(false),
            agent_role_registry: AgentRoleRegistry::load_default(),
        }
    }

    /// Attach an event bus for publishing domain events (e.g. DecisionCreated).
    pub fn with_event_bus(mut self, event_bus: Arc<InMemoryEventBus>) -> Self {
        self.event_bus = Some(event_bus);
        self
    }

    /// 消费并返回 checkpoint 信号，随后重置为 false。
    pub fn take_checkpoint_signal(&self) -> bool {
        self.checkpoint_signal.swap(false, Ordering::Relaxed)
    }

    /// 设置 checkpoint 信号（内部使用）。
    fn set_checkpoint_signal(&self) {
        self.checkpoint_signal.store(true, Ordering::Relaxed);
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

        // Step 1: Collect and handle expired leases (isolated by root_task_id).
        let expired = self.store.collect_expired_leases(root_task_id);
        for (task_id, lease_id) in &expired {
            self.store.revoke_lease(task_id, lease_id);
            if let Some(task) = self.store.get_task(task_id) {
                let next_status = if self.task_needs_evidence_after_lease_expiry(&task) {
                    TaskStatus::Blocked
                } else {
                    TaskStatus::Ready
                };
                if let Err(e) = self.store.update_status(task_id, next_status) {
                    return RunCycleOutcome::Error(format!(
                        "failed to reset expired-lease task {task_id}: {e}"
                    ));
                }
            }
        }

        // Step 1.5: Heartbeat all active leases to prevent premature expiry.
        let active_leases = self.store.collect_active_leases(root_task_id);
        let has_active_leases = !active_leases.is_empty();
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
            if has_active_leases {
                return RunCycleOutcome::Continue;
            }
            // Nothing runnable, no active worker lease, and not all complete — we're blocked.
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

            if !matches!(
                task.kind,
                TaskKind::Action | TaskKind::Validation | TaskKind::Repair
            ) {
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

            // Exclusive scope conflict check via executor_binding JSON metadata.
            if let Some(exc_scope) = task.executor_binding_exclusive_scope() {
                if self.has_running_exclusive_scope(root_task_id, exc_scope, &task.task_id) {
                    unmatched_ids.push(task.task_id.clone());
                    continue;
                }
            }
            // Parallelism group conflict: same group cannot run concurrently (design 5.3).
            if let Some(group) = task.executor_binding_parallelism_group() {
                if running_parallelism_groups.contains(group) {
                    unmatched_ids.push(task.task_id.clone());
                    continue;
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
                match self.check_policy_allows_dispatch(policy, task) {
                    DispatchPolicyOutcome::Allow => {}
                    DispatchPolicyOutcome::Reject(_) => {
                        unmatched_ids.push(task.task_id.clone());
                        continue;
                    }
                    DispatchPolicyOutcome::NeedsApproval(ref reason) => {
                        // 交互模式下等待用户确认：创建 Decision Task 阻塞当前任务。
                        if let Some(parent_id) = &task.parent_task_id {
                            let payload = serde_json::json!({
                                "decision_context": reason,
                                "blocked_reason": format!("任务 {} 等待继续执行确认", task.title),
                                "target_task_id": task.task_id.to_string(),
                                "options": [
                                    {"option_id": "continue", "label": "继续执行", "description": "继续执行后续任务"},
                                    {"option_id": "skip", "label": "跳过此任务", "description": "跳过此任务继续后续流程"},
                                    {"option_id": "cancel", "label": "取消整个任务", "description": "取消整个任务树"}
                                ],
                                "risk_notes": ["交互模式要求用户确认每一步"],
                                "recommended_option": "continue",
                                "required_user_input": true,
                                "decision_evidence": null
                            });
                            let _ = self.escalate_to_decision(parent_id, payload);
                        }
                        unmatched_ids.push(task.task_id.clone());
                        continue;
                    }
                }
            }

            let Some(required_role) = resolve_task_role(task, &self.agent_role_registry) else {
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
                    &task.root_task_id,
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
                    if let Some(group) = task.executor_binding_parallelism_group() {
                        running_parallelism_groups.insert(group.to_string());
                    }
                }
                // If grant_lease returns None the task already has an active
                // lease — skip it silently.
            } else {
                unmatched_ids.push(task.task_id.clone());
            }
        }

        if dispatched == 0 && !unmatched_ids.is_empty() {
            if has_active_leases {
                return RunCycleOutcome::Continue;
            }
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
                        self.record_result_evidence(&result.task_id, output_refs);
                    }
                    if let Some(task) = self.store.get_task(&result.task_id) {
                        if self.requires_delivery_evidence(&task) && output_refs.is_empty() {
                            self.escalate_missing_evidence(&task)?;
                            TaskStatus::Blocked
                        } else if task.kind != TaskKind::Validation
                            && task
                                .policy_snapshot
                                .as_ref()
                                .and_then(|p| p.validation_profile.as_ref())
                                .is_some()
                            && !self.store.has_validation_dependent(&result.task_id)
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
                        self.record_result_evidence(&result.task_id, output_refs);
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
                                variant: magi_core::TaskVariant::default(),
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
            if is_terminal(next_status) {
                self.set_checkpoint_signal();
            }

            // Auto-create Validation child when entering Verifying state.
            if next_status == TaskStatus::Verifying {
                self.create_validation_child(&result.task_id);
            }

            // Repair 子任务完成后，释放父任务从 Repairing 回到 Ready/Verifying。
            if next_status == TaskStatus::Completed || next_status == TaskStatus::Verifying {
                if let Some(task) = self.store.get_task(&result.task_id) {
                    if task.kind == TaskKind::Repair {
                        if let Some(parent_id) = &task.parent_task_id {
                            if let Some(parent) = self.store.get_task(parent_id) {
                                if parent.status == TaskStatus::Repairing {
                                    let parent_next = if parent
                                        .policy_snapshot
                                        .as_ref()
                                        .and_then(|p| p.validation_profile.as_ref())
                                        .is_some()
                                        && !self.store.has_validation_dependent(parent_id)
                                    {
                                        TaskStatus::Verifying
                                    } else {
                                        TaskStatus::Ready
                                    };
                                    if let Err(e) = self.store.update_status(parent_id, parent_next)
                                    {
                                        tracing::warn!(
                                            parent_id = %parent_id,
                                            error = %e,
                                            "failed to release parent from Repairing"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
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

    fn escalation_condition_label(condition: &str) -> Option<&'static str> {
        match condition {
            "on_failure" => Some("执行失败"),
            "high_risk" => Some("高风险操作"),
            "on_repair_exhausted" => Some("修复次数耗尽"),
            "repair_budget_exhausted" => Some("修复预算耗尽"),
            "conflicting_requirements" => Some("需求冲突"),
            "architecture_fork" => Some("架构分歧"),
            "missing_acceptance_criteria" => Some("验收标准缺失"),
            "unsafe_or_destructive_action" => Some("安全或破坏性风险"),
            "permission_boundary" => Some("权限边界"),
            "irreversible_action" => Some("不可逆操作"),
            _ => None,
        }
    }

    fn join_condition_labels(labels: &[&'static str]) -> String {
        match labels {
            [] => String::new(),
            [only] => (*only).to_string(),
            [left, right] => format!("{left}和{right}"),
            _ => {
                let mut joined = labels[..labels.len() - 1].join("、");
                joined.push_str("和");
                joined.push_str(labels[labels.len() - 1]);
                joined
            }
        }
    }

    fn summarize_escalation_conditions(conditions: &[String]) -> String {
        let labels: Vec<&'static str> = conditions
            .iter()
            .filter_map(|condition| Self::escalation_condition_label(condition))
            .collect();
        if labels.is_empty() {
            return "任务执行失败，需要确认后续处理方式。".to_string();
        }
        format!(
            "涉及{}，需要确认后续处理方式。",
            Self::join_condition_labels(&labels)
        )
    }

    fn build_decision_task_title(decision_context: &str) -> String {
        let context = decision_context
            .trim()
            .strip_prefix("Decision:")
            .unwrap_or(decision_context)
            .trim();
        if context.starts_with("需要决策") {
            context.to_string()
        } else {
            format!("需要决策：{context}")
        }
    }

    fn record_result_evidence(&self, task_id: &TaskId, output_refs: &[String]) {
        if output_refs.is_empty() {
            return;
        }
        let evidence_refs = output_refs
            .iter()
            .enumerate()
            .map(|(index, output_ref)| {
                format!("evidence://task/{task_id}/output/{index}?ref={output_ref}")
            })
            .collect();
        self.store.set_evidence_refs(task_id, evidence_refs);
    }

    /// 深度模式下，Action/Validation/Repair 完成时必须产出交付证据。
    fn requires_delivery_evidence(&self, task: &Task) -> bool {
        matches!(
            task.kind,
            TaskKind::Action | TaskKind::Validation | TaskKind::Repair
        ) && task
            .policy_snapshot
            .as_ref()
            .is_some_and(|policy| policy.validation_profile.is_some() && policy.background_allowed)
    }

    fn task_needs_evidence_after_lease_expiry(&self, task: &Task) -> bool {
        matches!(
            task.kind,
            TaskKind::Action | TaskKind::Validation | TaskKind::Repair
        ) && task
            .policy_snapshot
            .as_ref()
            .is_some_and(|policy| policy.validation_profile.is_some())
            && task.output_refs.is_empty()
            && task.evidence_refs.is_empty()
    }

    fn escalate_missing_evidence(&self, task: &Task) -> Result<(), String> {
        let Some(parent_id) = &task.parent_task_id else {
            return Ok(());
        };
        let payload = serde_json::json!({
            "decision_context": format!("验证任务 {} 缺少交付证据", task.title),
            "blocked_reason": "深度模式验证任务完成时未产出 output_refs，无法确认交付质量",
            "target_task_id": task.task_id.to_string(),
            "options": [
                {"option_id": "provide_evidence", "label": "补充证据", "description": "补充验证证据后继续推进"},
                {"option_id": "rerun_validation", "label": "重新验证", "description": "重新执行验证任务以生成证据"},
                {"option_id": "abort", "label": "中止", "description": "中止当前任务链"}
            ],
            "risk_notes": ["缺少验证证据会导致深度模式交付不可审计"],
            "recommended_option": "rerun_validation",
            "required_user_input": true,
            "decision_evidence": null
        });
        self.escalate_to_decision(parent_id, payload).map(|_| ())
    }

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
            variant: magi_core::TaskVariant::default(),
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
        let should_escalate = conditions.iter().any(|condition| {
            matches!(
                condition.as_str(),
                "on_failure"
                    | "high_risk"
                    | "on_repair_exhausted"
                    | "repair_budget_exhausted"
                    | "conflicting_requirements"
                    | "architecture_fork"
                    | "missing_acceptance_criteria"
                    | "unsafe_or_destructive_action"
                    | "permission_boundary"
                    | "irreversible_action"
            )
        });
        if !should_escalate {
            return;
        }
        let Some(parent_id) = &task.parent_task_id else {
            return;
        };
        let risk_notes: Vec<String> = conditions
            .iter()
            .filter_map(|condition| {
                Self::escalation_condition_label(condition)
                    .map(|label| format!("触发风险：{label}"))
            })
            .collect();
        let payload = serde_json::json!({
            "decision_context": format!("{} 执行失败，需要选择后续处理方式", task.title),
            "blocked_reason": format!(
                "失败原因：{}",
                Self::summarize_escalation_conditions(&conditions)
            ),
            "target_task_id": task.task_id.to_string(),
            "options": [
                {"option_id": "retry", "label": "重试", "description": "重新执行失败的任务"},
                {"option_id": "skip", "label": "跳过", "description": "跳过此任务继续后续流程"},
                {"option_id": "abort", "label": "中止", "description": "中止整个任务树"}
            ],
            "risk_notes": risk_notes,
            "recommended_option": "retry",
            "required_user_input": true,
            "decision_evidence": null
        });
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

                if is_terminal(task.status)
                    || (task.status == TaskStatus::Ready
                        && !matches!(
                            task.kind,
                            TaskKind::Objective | TaskKind::Phase | TaskKind::WorkPackage
                        ))
                {
                    // 可执行 Ready 任务尚未被调度或刚从 Repairing 释放，不应被自动完成。
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
                    if is_terminal(next_status) {
                        self.set_checkpoint_signal();
                    }
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
                        && t.executor_binding_exclusive_scope() == Some(scope)
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
                        t.executor_binding_parallelism_group().map(ToOwned::to_owned)
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    /// Check if the task's policy allows dispatch.
    fn check_policy_allows_dispatch(
        &self,
        policy: &magi_core::TaskPolicy,
        task: &Task,
    ) -> DispatchPolicyOutcome {
        // Manual  autonomy_level: 完全禁止自动派发。
        if policy.autonomy_level.eq_ignore_ascii_case("manual") {
            return DispatchPolicyOutcome::Reject(
                "autonomy_level 为 Manual，不允许自动派发".to_string(),
            );
        }

        // DecisionOnly: 只允许 Action/Validation/Repair 自动派发，
        // 但需要检查同级前序 Action 是否需要交互确认。
        if policy.approval_mode.eq_ignore_ascii_case("decisiononly") {
            return DispatchPolicyOutcome::Allow;
        }

        // Interactive: Action 完成后下一个同级 Action 需要用户确认。
        // 这里检查"同级前序 Action 是否刚完成"，如果是则创建 Decision。
        if policy.approval_mode.eq_ignore_ascii_case("interactive")
            && matches!(
                task.kind,
                TaskKind::Action | TaskKind::Validation | TaskKind::Repair
            )
        {
            // 检查同级前序 Action 是否有最近完成的
            if let Some(ref parent_id) = task.parent_task_id {
                let siblings = self.store.get_children(parent_id);
                let has_recent_completed_sibling = siblings.iter().any(|sibling| {
                    if sibling.task_id == task.task_id {
                        return false;
                    }
                    matches!(
                        sibling.kind,
                        TaskKind::Action | TaskKind::Validation | TaskKind::Repair
                    ) && matches!(
                        sibling.status,
                        TaskStatus::Completed | TaskStatus::Verifying
                    )
                });
                if has_recent_completed_sibling {
                    return DispatchPolicyOutcome::NeedsApproval(
                        "交互模式：等待用户确认继续".to_string(),
                    );
                }
            }
        }

        DispatchPolicyOutcome::Allow
    }

    // ------------------------------------------------------------------
    // G4: Decision Task lifecycle (design 7.x)
    // ------------------------------------------------------------------

    /// Create a Decision task as a child of the given task, blocking the parent
    /// until the decision is resolved (design 7.x).
    pub fn escalate_to_decision(
        &self,
        parent_task_id: &TaskId,
        payload: serde_json::Value,
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
        let event_context = decision_payload_text(&payload, "decision_context");
        let blocked_reason = decision_payload_text(&payload, "blocked_reason");
        let event_options: Vec<String> = payload
            .get("options")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|option| option.get("option_id").and_then(serde_json::Value::as_str))
            .map(ToOwned::to_owned)
            .collect();
        let decision_task = Task {
            task_id: decision_id.clone(),
            mission_id: parent.mission_id.clone(),
            root_task_id: parent.root_task_id.clone(),
            parent_task_id: Some(parent_task_id.clone()),
            kind: TaskKind::Decision,
            title: Self::build_decision_task_title(&event_context),
            goal: blocked_reason,
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
            variant: magi_core::TaskVariant::default(),
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        };
        self.store.insert_task(decision_task);
        self.store
            .update_status(parent_task_id, TaskStatus::Blocked)
            .map_err(|e| format!("failed to block parent {parent_task_id}: {e}"))?;

        if let Some(ref event_bus) = self.event_bus {
            let event = EventEnvelope::domain(
                EventId::new(format!("event-decision-created-{}", UtcMillis::now().0)),
                "task.decision.created",
                serde_json::json!({
                    "session_id": parent.mission_id.to_string(),
                    "task_id": decision_id.to_string(),
                    "parent_task_id": parent_task_id.to_string(),
                    "context": event_context,
                    "options": event_options,
                }),
            );
            let _ = event_bus.publish(event);
        }

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

    /// 将 runner 已确认无法继续自动推进的 blocked outcome 写回任务图事实源。
    ///
    /// `RunCycleOutcome::Blocked` 只描述一次调度周期的原因；后台 runner 在连续确认
    /// blocked 后退出时，必须把对应任务原位收束为 `Blocked`，否则 graph projection 会
    /// 同时出现 runner 已阻塞、task store 仍 Running/Ready 的双事实。
    pub fn finalize_blocked_outcome(
        &self,
        root_task_id: &TaskId,
        blocked_task_ids: &[TaskId],
    ) -> Result<u32, String> {
        let mut changed = 0u32;
        let candidates = if blocked_task_ids.is_empty() {
            self.collect_non_terminal_task_ids(root_task_id)
        } else {
            blocked_task_ids.to_vec()
        };

        for task_id in candidates {
            let Some(task) = self.store.get_task(&task_id) else {
                continue;
            };
            if !matches!(
                task.status,
                TaskStatus::Draft
                    | TaskStatus::Ready
                    | TaskStatus::Running
                    | TaskStatus::Verifying
                    | TaskStatus::Repairing
            ) {
                continue;
            }
            self.store
                .update_status(&task_id, TaskStatus::Blocked)
                .map_err(|e| format!("failed to finalize blocked task {task_id}: {e}"))?;
            if let Some(lease) = self.store.get_active_lease(&task_id) {
                self.store.revoke_lease(&task_id, &lease.lease_id);
            }
            changed += 1;
        }

        self.propagate_parent_completion(root_task_id)?;
        if changed > 0 {
            self.set_checkpoint_signal();
        }
        Ok(changed)
    }

    // ------------------------------------------------------------------
    // G5: Control commands (design 9.x)
    // ------------------------------------------------------------------

    /// Pause a running task and all its non-terminal可调度 descendants.
    pub fn pause_task(&self, task_id: &TaskId) -> Result<(), String> {
        let task = self.store.get_task(task_id).ok_or("task not found")?;
        if task.status == TaskStatus::Blocked {
            return Ok(());
        }
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
        LeaseId, MissionId, Task, TaskId, TaskKind, TaskPolicy, TaskResultKind, TaskStatus,
        TerminationReason, UtcMillis, VerificationStatus, WorkerId,
    };
    use magi_orchestrator::task_store::{TaskLease, TaskLeaseState};
    use magi_worker_runtime::{WorkerExecutionIntentStep, WorkerRuntime};

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
            variant: magi_core::TaskVariant::default(),
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
            system_prompt_template: None,
        }
    }

    #[test]
    fn worker_execution_intent_final_report_is_success_when_generated_steps_succeed() {
        let dispatcher = WorkerExecutionDispatcher::new(
            WorkerRuntime::new(Arc::new(InMemoryEventBus::new(64))),
            Arc::new(EventBasedResultReceiver::new()),
        );
        let task = make_task(
            "validation-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Validation,
            TaskStatus::Ready,
        );
        let worker = make_worker("worker-reviewer", "reviewer", vec![TaskKind::Validation]);

        let intent = dispatcher.build_intent_from_task(&task, &worker);
        let final_report = intent
            .steps
            .iter()
            .find_map(|step| match step {
                WorkerExecutionIntentStep::FinalReport(report) => Some(report),
                _ => None,
            })
            .expect("generated worker intent should include final report");

        assert_eq!(final_report.result_kind, Some(TaskResultKind::Success));
        assert_eq!(
            final_report.termination_reason,
            Some(TerminationReason::Completed)
        );
        assert_eq!(final_report.verification_status, VerificationStatus::Passed);
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
        let expired_lease = TaskLease {
            lease_id: LeaseId::new("lease-expired"),
            task_id: TaskId::new("act-1"),
            root_task_id: TaskId::new("obj-1"),
            worker_id: WorkerId::new("w-1"),
            role: "executor".to_string(),
            granted_at: UtcMillis(now.0.saturating_sub(120_000)),
            expires_at: UtcMillis(now.0.saturating_sub(60_000)),
            heartbeat_at: UtcMillis(now.0.saturating_sub(120_000)),
            lease_status: TaskLeaseState::Active,
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

    #[test]
    fn ready_structural_parent_completed_when_all_children_complete() {
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
            TaskStatus::Ready,
        );
        let work_package = make_task(
            "wp-1",
            "m-1",
            "obj-1",
            Some("phase-1"),
            TaskKind::WorkPackage,
            TaskStatus::Ready,
        );
        let action = make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("wp-1"),
            TaskKind::Action,
            TaskStatus::Completed,
        );
        let validation = make_task(
            "validation-1",
            "m-1",
            "obj-1",
            Some("wp-1"),
            TaskKind::Validation,
            TaskStatus::Completed,
        );

        store.insert_task(root);
        store.insert_task(phase);
        store.insert_task(work_package);
        store.insert_task(action);
        store.insert_task(validation);

        let runner = TaskRunner::new(Arc::clone(&store), Vec::new());

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(outcome, RunCycleOutcome::AllComplete);
        assert_eq!(
            store.get_task(&TaskId::new("wp-1")).unwrap().status,
            TaskStatus::Completed
        );
        assert_eq!(
            store.get_task(&TaskId::new("phase-1")).unwrap().status,
            TaskStatus::Completed
        );
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

    #[test]
    fn finalize_blocked_outcome_writes_task_graph_fact() {
        let store = Arc::new(TaskStore::new());

        store.insert_task(make_task(
            "obj-1",
            "m-1",
            "obj-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        ));
        store.insert_task(make_task(
            "act-1",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        ));

        let runner = TaskRunner::new(Arc::clone(&store), Vec::new());
        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        let blocked_ids = match outcome {
            RunCycleOutcome::Blocked(ids) => ids,
            other => panic!("expected Blocked, got {:?}", other),
        };

        runner
            .finalize_blocked_outcome(&TaskId::new("obj-1"), &blocked_ids)
            .expect("blocked outcome should be persisted into task graph");

        let projection = store.build_projection(&TaskId::new("obj-1")).unwrap();
        assert_eq!(projection.runner_status, "blocked");
        assert_eq!(projection.aggregate_status, TaskStatus::Blocked);
        assert!(projection.running_tasks.is_empty());
        assert_eq!(projection.progress_summary.running_tasks, 0);
        assert!(
            projection
                .blocked_tasks
                .iter()
                .any(|id| id == &TaskId::new("act-1"))
        );
        assert_eq!(
            store.get_task(&TaskId::new("obj-1")).unwrap().status,
            TaskStatus::Blocked
        );
    }

    #[test]
    fn runner_does_not_block_while_active_lease_is_in_flight() {
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
        store
            .grant_lease(
                &TaskId::new("act-1"),
                &TaskId::new("obj-1"),
                &WorkerId::new("worker-dev"),
                "integration-dev",
                60_000,
            )
            .expect("running task should hold an active lease");

        let runner = TaskRunner::new(Arc::clone(&store), Vec::new());

        let outcome = runner.run_cycle(&TaskId::new("obj-1"));
        assert_eq!(
            outcome,
            RunCycleOutcome::Continue,
            "已有活跃租约代表 worker 仍在执行，runner 不应误判为 blocked"
        );
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
            lease: &TaskLease,
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
        let lease = TaskLease {
            lease_id: LeaseId::new("lease-act-1"),
            task_id: TaskId::new("act-1"),
            root_task_id: TaskId::new("obj-1"),
            worker_id: WorkerId::new("w-1"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0 + 120_000),
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
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
        let lease = TaskLease {
            lease_id: LeaseId::new("lease-act-1"),
            task_id: TaskId::new("act-1"),
            root_task_id: TaskId::new("obj-1"),
            worker_id: WorkerId::new("w-1"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0 + 120_000),
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
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
        let lease = TaskLease {
            lease_id: LeaseId::new("lease-act-1"),
            task_id: TaskId::new("act-1"),
            root_task_id: TaskId::new("obj-1"),
            worker_id: WorkerId::new("w-1"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0 + 120_000),
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
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
            _lease: &TaskLease,
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
        act1.executor_binding = Some(serde_json::json!({
            "target_role": "integration-dev",
            "capability_requirements": [],
            "parallelism_group": null,
            "exclusive_scope": "deploy-prod",
            "worker_selector": null,
        }));
        let mut act2 = make_task(
            "act-2",
            "m-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        act2.executor_binding = Some(serde_json::json!({
            "target_role": "integration-dev",
            "capability_requirements": [],
            "parallelism_group": null,
            "exclusive_scope": "deploy-prod",
            "worker_selector": null,
        }));

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
        let lease = TaskLease {
            lease_id: LeaseId::new("lease-act-1"),
            task_id: TaskId::new("act-1"),
            root_task_id: TaskId::new("obj-1"),
            worker_id: WorkerId::new("w-1"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0 + 120_000),
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
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
        let lease = TaskLease {
            lease_id: LeaseId::new("lease-act-1"),
            task_id: TaskId::new("act-1"),
            root_task_id: TaskId::new("obj-1"),
            worker_id: WorkerId::new("w-1"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0 + 120_000),
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
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

    // -----------------------------------------------------------------------
    // Test: Repair 子任务完成后释放父任务从 Repairing → Ready
    // -----------------------------------------------------------------------

    #[test]
    fn repair_completion_releases_parent_from_repairing() {
        let store = Arc::new(TaskStore::new());
        let _runner = TaskRunner::new(
            store.clone(),
            vec![
                WorkerInfo {
                    worker_id: WorkerId::new("worker-debugger"),
                    role: "debugger".to_string(),
                    supported_kinds: vec![TaskKind::Repair],
                    parallelism_limit: None,
                    system_prompt_template: None,
                },
                WorkerInfo {
                    worker_id: WorkerId::new("worker-dev"),
                    role: "integration-dev".to_string(),
                    supported_kinds: vec![TaskKind::Action],
                    parallelism_limit: None,
                    system_prompt_template: None,
                },
            ],
        );

        let parent = Task {
            task_id: TaskId::new("parent-1"),
            mission_id: MissionId::new("m-1"),
            root_task_id: TaskId::new("parent-1"),
            parent_task_id: None,
            kind: TaskKind::Action,
            title: "Parent action".to_string(),
            goal: "Do something".to_string(),
            status: TaskStatus::Repairing,
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
            repair_count: 1,
            decision_payload: None,
            variant: magi_core::TaskVariant::default(),
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        };
        store.insert_task(parent);

        let repair = Task {
            task_id: TaskId::new("repair-1"),
            mission_id: MissionId::new("m-1"),
            root_task_id: TaskId::new("parent-1"),
            parent_task_id: Some(TaskId::new("parent-1")),
            kind: TaskKind::Repair,
            title: "Repair task".to_string(),
            goal: "Fix failure".to_string(),
            status: TaskStatus::Running,
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
            variant: magi_core::TaskVariant::default(),
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        };
        store.insert_task(repair);

        // Grant lease so apply_results can complete it.
        let lease = store
            .grant_lease(
                &TaskId::new("repair-1"),
                &TaskId::new("parent-1"),
                &WorkerId::new("worker-debugger"),
                "debugger",
                60_000,
            )
            .unwrap();

        // Push a Completed result for the repair task.
        let receiver = Arc::new(EventBasedResultReceiver::new());
        let runner = TaskRunner::with_dispatcher(
            store.clone(),
            vec![
                WorkerInfo {
                    worker_id: WorkerId::new("worker-debugger"),
                    role: "debugger".to_string(),
                    supported_kinds: vec![TaskKind::Repair],
                    parallelism_limit: None,
                    system_prompt_template: None,
                },
                WorkerInfo {
                    worker_id: WorkerId::new("worker-dev"),
                    role: "integration-dev".to_string(),
                    supported_kinds: vec![TaskKind::Action],
                    parallelism_limit: None,
                    system_prompt_template: None,
                },
            ],
            Arc::new(NoOpDispatcher),
            receiver.clone(),
        );

        receiver.push_result(TaskResult {
            task_id: TaskId::new("repair-1"),
            lease_id: lease.lease_id,
            outcome: TaskOutcome::Completed {
                output_refs: vec!["fixed".to_string()],
            },
        });

        let outcome = runner.run_cycle(&TaskId::new("parent-1"));
        assert_eq!(outcome, RunCycleOutcome::Continue);

        // Repair task should be Completed.
        assert_eq!(
            store.get_task(&TaskId::new("repair-1")).unwrap().status,
            TaskStatus::Completed
        );
        // Parent should be released from Repairing → Ready → Running (dispatched in same cycle).
        assert_eq!(
            store.get_task(&TaskId::new("parent-1")).unwrap().status,
            TaskStatus::Running
        );
    }

    // -----------------------------------------------------------------------
    // Test: approval_mode == "Interactive" 时，下一个同级 Action 触发 Decision
    // -----------------------------------------------------------------------

    #[test]
    fn approval_mode_interactive_creates_decision_for_next_action() {
        let store = Arc::new(TaskStore::new());
        let policy = TaskPolicy {
            autonomy_level: "Assisted".to_string(),
            approval_mode: "Interactive".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 1,
            repair_limit: 1,
            validation_profile: None,
            checkpoint_mode: "turn".to_string(),
            background_allowed: false,
            escalation_conditions: vec!["on_failure".to_string()],
        };

        let parent = make_task(
            "parent-1",
            "m-1",
            "parent-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        store.insert_task(parent);

        let mut action1 = make_task(
            "action-1",
            "m-1",
            "parent-1",
            Some("parent-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        action1.policy_snapshot = Some(policy.clone());
        store.insert_task(action1);

        let mut action2 = make_task(
            "action-2",
            "m-1",
            "parent-1",
            Some("parent-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        action2.policy_snapshot = Some(policy.clone());
        store.insert_task(action2);

        let receiver = Arc::new(EventBasedResultReceiver::new());
        let runner = TaskRunner::with_dispatcher(
            store.clone(),
            vec![make_worker(
                "worker-dev",
                "integration-dev",
                vec![TaskKind::Action],
            )],
            Arc::new(NoOpDispatcher),
            receiver.clone(),
        );

        // action-1 完成
        let lease1 = store
            .grant_lease(
                &TaskId::new("action-1"),
                &TaskId::new("parent-1"),
                &WorkerId::new("worker-dev"),
                "integration-dev",
                60_000,
            )
            .unwrap();
        receiver.push_result(TaskResult {
            task_id: TaskId::new("action-1"),
            lease_id: lease1.lease_id,
            outcome: TaskOutcome::Completed {
                output_refs: vec!["out1".to_string()],
            },
        });

        // 一个 cycle 内：处理 action-1 完成，然后尝试 dispatch action-2，
        // Interactive 模式下触发 NeedsApproval，创建 Decision 并 Block parent
        let outcome = runner.run_cycle(&TaskId::new("parent-1"));
        assert!(
            matches!(outcome, RunCycleOutcome::Blocked(ref ids) if ids.contains(&TaskId::new("action-2"))),
            "Interactive 模式下 action-2 应被拦截，返回 Blocked"
        );
        assert_eq!(
            store.get_task(&TaskId::new("action-1")).unwrap().status,
            TaskStatus::Completed
        );

        let parent_task = store.get_task(&TaskId::new("parent-1")).unwrap();
        assert_eq!(parent_task.status, TaskStatus::Blocked);

        let children = store.get_children(&TaskId::new("parent-1"));
        let decision = children.iter().find(|t| t.kind == TaskKind::Decision);
        assert!(decision.is_some(), "Interactive 模式下应创建 Decision Task");
        let decision = decision.unwrap();
        assert_eq!(decision.status, TaskStatus::AwaitingApproval);
        assert!(decision.decision_payload.is_some());
    }

    // -----------------------------------------------------------------------
    // Test: approval_mode == "DecisionOnly" 时，同级 Action 直接允许 dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn approval_mode_decision_only_allows_dispatch_without_decision() {
        let store = Arc::new(TaskStore::new());
        let policy = TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 3,
            repair_limit: 3,
            validation_profile: Some("Required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            background_allowed: true,
            escalation_conditions: vec![
                "on_failure".to_string(),
                "repair_budget_exhausted".to_string(),
            ],
        };

        let parent = make_task(
            "parent-1",
            "m-1",
            "parent-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        store.insert_task(parent);

        let mut action1 = make_task(
            "action-1",
            "m-1",
            "parent-1",
            Some("parent-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        action1.policy_snapshot = Some(policy.clone());
        store.insert_task(action1);

        let mut action2 = make_task(
            "action-2",
            "m-1",
            "parent-1",
            Some("parent-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        action2.policy_snapshot = Some(policy.clone());
        store.insert_task(action2);

        let receiver = Arc::new(EventBasedResultReceiver::new());
        let runner = TaskRunner::with_dispatcher(
            store.clone(),
            vec![
                make_worker("worker-dev", "integration-dev", vec![TaskKind::Action]),
                make_worker("worker-reviewer", "reviewer", vec![TaskKind::Validation]),
            ],
            Arc::new(NoOpDispatcher),
            receiver.clone(),
        );

        // action-1 完成
        let lease1 = store
            .grant_lease(
                &TaskId::new("action-1"),
                &TaskId::new("parent-1"),
                &WorkerId::new("worker-dev"),
                "integration-dev",
                60_000,
            )
            .unwrap();
        receiver.push_result(TaskResult {
            task_id: TaskId::new("action-1"),
            lease_id: lease1.lease_id,
            outcome: TaskOutcome::Completed {
                output_refs: vec!["out1".to_string()],
            },
        });

        // 一个 cycle 内处理 action-1 完成并 dispatch action-2
        let outcome = runner.run_cycle(&TaskId::new("parent-1"));
        assert_eq!(outcome, RunCycleOutcome::Continue);
        assert_eq!(
            store.get_task(&TaskId::new("action-1")).unwrap().status,
            TaskStatus::Verifying
        );
        assert_eq!(
            store.get_task(&TaskId::new("action-2")).unwrap().status,
            TaskStatus::Running
        );

        let children = store.get_children(&TaskId::new("parent-1"));
        let decision_count = children
            .iter()
            .filter(|t| t.kind == TaskKind::Decision)
            .count();
        assert_eq!(
            decision_count, 0,
            "DecisionOnly 模式下不应创建 Decision Task"
        );
    }

    // -----------------------------------------------------------------------
    // Test: 深度模式 evidence 约束 — 无 output_refs 时 Blocked
    // -----------------------------------------------------------------------

    #[test]
    fn deep_mode_evidence_required_blocks_without_output_refs() {
        let store = Arc::new(TaskStore::new());
        let policy = TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 3,
            repair_limit: 3,
            validation_profile: Some("Required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            background_allowed: true,
            escalation_conditions: vec!["on_failure".to_string()],
        };

        let parent = make_task(
            "parent-1",
            "m-1",
            "parent-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        store.insert_task(parent);

        let mut action = make_task(
            "action-1",
            "m-1",
            "parent-1",
            Some("parent-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        action.policy_snapshot = Some(policy);
        store.insert_task(action);

        let receiver = Arc::new(EventBasedResultReceiver::new());
        let runner = TaskRunner::with_dispatcher(
            store.clone(),
            vec![make_worker(
                "worker-dev",
                "integration-dev",
                vec![TaskKind::Action],
            )],
            Arc::new(NoOpDispatcher),
            receiver.clone(),
        );

        // action-1 完成但无 output_refs
        let lease = store
            .grant_lease(
                &TaskId::new("action-1"),
                &TaskId::new("parent-1"),
                &WorkerId::new("worker-dev"),
                "integration-dev",
                60_000,
            )
            .unwrap();
        receiver.push_result(TaskResult {
            task_id: TaskId::new("action-1"),
            lease_id: lease.lease_id,
            outcome: TaskOutcome::Completed {
                output_refs: vec![],
            },
        });

        let outcome = runner.run_cycle(&TaskId::new("parent-1"));
        assert!(
            matches!(outcome, RunCycleOutcome::Blocked(_)),
            "深度模式 Required 且无 output_refs 应返回 Blocked"
        );

        let action_task = store.get_task(&TaskId::new("action-1")).unwrap();
        assert_eq!(
            action_task.status,
            TaskStatus::Blocked,
            "深度模式 Required 且无 output_refs 应 Blocked"
        );

        let parent_task = store.get_task(&TaskId::new("parent-1")).unwrap();
        assert_eq!(parent_task.status, TaskStatus::Blocked);

        let children = store.get_children(&TaskId::new("parent-1"));
        let decision = children.iter().find(|t| t.kind == TaskKind::Decision);
        assert!(decision.is_some(), "应创建 Decision Task 要求补充证据");
    }

    // -----------------------------------------------------------------------
    // Test: 普通模式 validation_profile Required — 无 output_refs 时 Verifying
    // -----------------------------------------------------------------------

    #[test]
    fn normal_mode_validation_profile_verifying_without_output_refs() {
        let store = Arc::new(TaskStore::new());
        let policy = TaskPolicy {
            autonomy_level: "Assisted".to_string(),
            approval_mode: "Interactive".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 1,
            repair_limit: 1,
            validation_profile: Some("Required".to_string()),
            checkpoint_mode: "turn".to_string(),
            background_allowed: false,
            escalation_conditions: vec!["on_failure".to_string()],
        };

        let parent = make_task(
            "parent-1",
            "m-1",
            "parent-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        store.insert_task(parent);

        let mut action = make_task(
            "action-1",
            "m-1",
            "parent-1",
            Some("parent-1"),
            TaskKind::Action,
            TaskStatus::Ready,
        );
        action.policy_snapshot = Some(policy);
        store.insert_task(action);

        let receiver = Arc::new(EventBasedResultReceiver::new());
        let runner = TaskRunner::with_dispatcher(
            store.clone(),
            vec![
                make_worker("worker-dev", "integration-dev", vec![TaskKind::Action]),
                make_worker("worker-reviewer", "reviewer", vec![TaskKind::Validation]),
            ],
            Arc::new(NoOpDispatcher),
            receiver.clone(),
        );

        let lease = store
            .grant_lease(
                &TaskId::new("action-1"),
                &TaskId::new("parent-1"),
                &WorkerId::new("worker-dev"),
                "integration-dev",
                60_000,
            )
            .unwrap();
        receiver.push_result(TaskResult {
            task_id: TaskId::new("action-1"),
            lease_id: lease.lease_id,
            outcome: TaskOutcome::Completed {
                output_refs: vec![],
            },
        });

        let outcome = runner.run_cycle(&TaskId::new("parent-1"));
        assert_eq!(outcome, RunCycleOutcome::Continue);

        let action_task = store.get_task(&TaskId::new("action-1")).unwrap();
        assert_eq!(
            action_task.status,
            TaskStatus::Verifying,
            "普通模式 Required 且无 output_refs 且无 validation dependent 应 Verifying"
        );
    }

    // -----------------------------------------------------------------------
    // Test: escalate_to_decision 通过 event_bus 发布 task.decision.created
    // -----------------------------------------------------------------------

    #[test]
    fn escalate_to_decision_publishes_event_via_event_bus() {
        let store = Arc::new(TaskStore::new());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(64));

        let parent = make_task(
            "parent-1",
            "m-1",
            "parent-1",
            None,
            TaskKind::Objective,
            TaskStatus::Running,
        );
        store.insert_task(parent);

        let runner = TaskRunner::new(
            store.clone(),
            vec![make_worker(
                "worker-dev",
                "integration-dev",
                vec![TaskKind::Action],
            )],
        )
        .with_event_bus(Arc::clone(&event_bus));

        let payload = serde_json::json!({
            "decision_context": "测试决策",
            "blocked_reason": "需要确认",
            "target_task_id": null,
            "options": [{"option_id": "yes", "label": "是", "description": "确认"}],
            "risk_notes": [],
            "recommended_option": "yes",
            "required_user_input": true,
            "decision_evidence": null
        });

        let decision_id = runner
            .escalate_to_decision(&TaskId::new("parent-1"), payload)
            .unwrap();

        let events = event_bus.snapshot().recent_events;
        let decision_event = events
            .iter()
            .find(|e| e.event_type == "task.decision.created");
        assert!(
            decision_event.is_some(),
            "escalate_to_decision 应发布 task.decision.created 事件"
        );
        let decision_event = decision_event.unwrap();
        let payload = decision_event
            .payload
            .as_object()
            .expect("payload 应为 object");
        assert_eq!(
            payload.get("task_id").and_then(|v| v.as_str()),
            Some(decision_id.as_str())
        );
    }
}
