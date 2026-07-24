//! 任务系统 — TaskRunner 调度循环归属 conversation-runtime。
//!
//! 本模块只维护任务执行事实：pending/running/terminal 状态推进、
//! worker 匹配、租约、结果回收。

use crate::execution_admission::ExecutionAdmissionController;
use crate::task_runner_bridge::{
    RunCycleOutcome, TaskDispatchGate, TaskDispatchGateDecision, TaskDispatcher, TaskOutcome,
    TaskResultReceiver,
};
use magi_agent_role::AgentRoleRegistry;
use magi_core::{SessionId, Task, TaskId, TaskStatus};
use magi_event_bus::InMemoryEventBus;
use magi_orchestrator::{
    task_store::TaskStore,
    task_worker_catalog::{WorkerInfo, resolve_task_role},
};
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const DEFAULT_LEASE_DURATION_MS: u64 = 60_000;

pub struct TaskRunner {
    store: Arc<TaskStore>,
    workers: Vec<WorkerInfo>,
    dispatcher: Arc<dyn TaskDispatcher>,
    result_receiver: Arc<dyn TaskResultReceiver>,
    dispatch_gate: Option<Arc<TaskDispatchGate>>,
    execution_admission: Arc<ExecutionAdmissionController>,
    session_id: Option<SessionId>,
    event_bus: Option<Arc<InMemoryEventBus>>,
    checkpoint_signal: AtomicBool,
    agent_role_registry: AgentRoleRegistry,
}

impl TaskRunner {
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
            dispatch_gate: None,
            execution_admission: Arc::new(ExecutionAdmissionController::default()),
            session_id: None,
            event_bus: None,
            checkpoint_signal: AtomicBool::new(false),
            agent_role_registry: AgentRoleRegistry::load_default(),
        }
    }

    pub fn with_agent_role_registry(mut self, registry: AgentRoleRegistry) -> Self {
        self.agent_role_registry = registry;
        self
    }

    pub fn with_event_bus(mut self, event_bus: Arc<InMemoryEventBus>) -> Self {
        self.event_bus = Some(event_bus);
        self
    }

    pub fn with_dispatch_gate(mut self, gate: Arc<TaskDispatchGate>) -> Self {
        self.dispatch_gate = Some(gate);
        self
    }

    pub fn with_execution_admission(
        mut self,
        execution_admission: Arc<ExecutionAdmissionController>,
        session_id: Option<SessionId>,
    ) -> Self {
        self.execution_admission = execution_admission;
        self.session_id = session_id;
        self
    }

    pub fn take_checkpoint_signal(&self) -> bool {
        self.checkpoint_signal.swap(false, Ordering::Relaxed)
    }

    fn set_checkpoint_signal(&self) {
        self.checkpoint_signal.store(true, Ordering::Relaxed);
    }

    pub fn run_cycle(&self, root_task_id: &TaskId) -> RunCycleOutcome {
        if let Err(error) = self.apply_results() {
            return RunCycleOutcome::Error(error);
        }

        if let Err(error) = self.expire_stale_leases(root_task_id) {
            return RunCycleOutcome::Error(error);
        }

        let active_leases = self.store.collect_active_leases(root_task_id);
        for (task_id, lease_id) in &active_leases {
            self.store.heartbeat_lease(task_id, lease_id);
        }

        match self.terminal_state(root_task_id) {
            TerminalState::AllCompleted => return RunCycleOutcome::AllComplete,
            TerminalState::HasFailures(task_ids) => {
                return RunCycleOutcome::Error(format!("任务执行失败: {:?}", task_ids));
            }
            TerminalState::HasKilled(task_ids) => {
                return RunCycleOutcome::Error(format!("任务已终止: {:?}", task_ids));
            }
            TerminalState::NotTerminal => {}
        }

        let runnable = self.store.get_runnable_leaves(root_task_id);
        if runnable.is_empty() {
            if !active_leases.is_empty() {
                return RunCycleOutcome::Continue;
            }
            return RunCycleOutcome::Stalled(self.collect_non_terminal_task_ids(root_task_id));
        }

        let mut dispatched = 0usize;
        let mut unmatched = Vec::new();
        let mut admission_blocked = Vec::new();
        for task in runnable {
            if let Some(gate) = &self.dispatch_gate {
                match gate(&task) {
                    Ok(TaskDispatchGateDecision::Allow) => {}
                    Ok(TaskDispatchGateDecision::Blocked(reason)) => {
                        return RunCycleOutcome::Blocked {
                            task_ids: vec![task.task_id.clone()],
                            reason,
                        };
                    }
                    Err(error) => {
                        return RunCycleOutcome::Error(format!(
                            "任务 {} 派发检查失败: {error}",
                            task.task_id
                        ));
                    }
                }
            }
            let Some(worker) = self.match_worker(&task) else {
                unmatched.push(task.task_id.clone());
                continue;
            };
            let admission_permit = match self.execution_admission.acquire(
                task.task_id.clone(),
                self.session_id.clone(),
                worker.role.clone(),
            ) {
                Ok(permit) => permit,
                Err(blocked) => {
                    admission_blocked.push((task.task_id.clone(), blocked.reason));
                    continue;
                }
            };
            let Some(lease) = self.store.grant_lease(
                &task.task_id,
                root_task_id,
                &worker.worker_id,
                &worker.role,
                DEFAULT_LEASE_DURATION_MS,
            ) else {
                drop(admission_permit);
                continue;
            };
            if let Err(error) = self
                .store
                .update_status_checked(&task.task_id, TaskStatus::Running)
            {
                self.store.revoke_lease(&task.task_id, &lease.lease_id);
                return RunCycleOutcome::Error(format!(
                    "任务 {} 进入 running 失败: {error}",
                    task.task_id
                ));
            }
            if let Err(error) = self
                .dispatcher
                .dispatch(&task, &worker, &lease, admission_permit)
            {
                self.store.revoke_lease(&task.task_id, &lease.lease_id);
                let _ = self.store.update_status(&task.task_id, TaskStatus::Failed);
                self.set_checkpoint_signal();
                return RunCycleOutcome::Error(format!("任务 {} 派发失败: {error}", task.task_id));
            }
            dispatched += 1;
        }

        if dispatched > 0 {
            RunCycleOutcome::Continue
        } else if let Some((task_id, reason)) = admission_blocked.into_iter().next() {
            RunCycleOutcome::Blocked {
                task_ids: vec![task_id],
                reason,
            }
        } else {
            RunCycleOutcome::Stalled(unmatched)
        }
    }

    pub fn finalize_stalled_outcome(
        &self,
        _root_task_id: &TaskId,
        task_ids: &[TaskId],
    ) -> Result<(), String> {
        for task_id in task_ids {
            if let Some(task) = self.store.get_task(task_id)
                && matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
            {
                if let Some(lease) = self.store.get_active_lease(task_id) {
                    self.store.revoke_lease(task_id, &lease.lease_id);
                }
                self.store
                    .set_output_refs(task_id, vec![self.stalled_task_reason(&task)]);
                self.store
                    .update_status(task_id, TaskStatus::Failed)
                    .map_err(|error| format!("收口不可运行任务 {task_id} 失败: {error}"))?;
            }
        }
        self.set_checkpoint_signal();
        Ok(())
    }

    pub fn finalize_unexpected_failure(
        &self,
        root_task_id: &TaskId,
        reason: &str,
    ) -> Result<(), String> {
        let task_ids = self.collect_subtree_ids(root_task_id);
        if task_ids.is_empty() {
            return Err(format!("任务树不存在: {root_task_id}"));
        }
        for task_id in task_ids {
            let Some(task) = self.store.get_task(&task_id) else {
                continue;
            };
            if matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
                if let Some(lease) = self.store.get_active_lease(&task_id) {
                    self.store.revoke_lease(&task_id, &lease.lease_id);
                }
                self.store
                    .set_output_refs(&task_id, vec![reason.to_string()]);
                self.store
                    .update_status(&task_id, TaskStatus::Failed)
                    .map_err(|error| format!("收口异常任务 {task_id} 失败: {error}"))?;
            }
        }
        self.set_checkpoint_signal();
        Ok(())
    }

    fn stalled_task_reason(&self, task: &Task) -> String {
        let role = resolve_task_role(task, &self.agent_role_registry)
            .or_else(|| task.executor_binding_target_role())
            .unwrap_or("unknown");
        if task.parent_task_id.is_some() {
            return format!(
                "代理不可用：没有匹配角色 {role} 的可用执行器。父代理应改派其他可用角色，或由主线根据已有上下文继续完成。"
            );
        }
        format!("任务不可运行：没有匹配角色 {role} 的可用执行器。")
    }

    pub fn kill_task(&self, task_id: &TaskId) -> Result<(), String> {
        let task = self
            .store
            .get_task(task_id)
            .ok_or_else(|| format!("任务不存在: {task_id}"))?;
        if matches!(
            task.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
        ) {
            self.execution_admission.remove_queued_task(task_id);
            return Ok(());
        }
        if let Some(lease) = self.store.get_active_lease(task_id) {
            self.store.revoke_lease(task_id, &lease.lease_id);
        }
        self.store
            .update_status(task_id, TaskStatus::Killed)
            .map_err(|error| format!("终止任务 {task_id} 失败: {error}"))?;
        self.execution_admission.remove_queued_task(task_id);
        self.set_checkpoint_signal();
        Ok(())
    }

    pub fn kill_tree(&self, root_task_id: &TaskId) -> Result<(), String> {
        for task_id in self.collect_subtree_ids(root_task_id) {
            self.kill_task(&task_id)?;
        }
        Ok(())
    }

    pub fn resume_task(&self, task_id: &TaskId) -> Result<(), String> {
        let task = self
            .store
            .get_task(task_id)
            .ok_or_else(|| format!("任务不存在: {task_id}"))?;
        match task.status {
            // 已经在 Pending：等待 dispatcher 派发，无需动作
            TaskStatus::Pending => Ok(()),
            // 用户显式继续：把 Failed 任务回退到 Pending，由 dispatcher 重新派发
            // 这条路径是 `/api/session/continue` 的 root-status==Failed 入口
            TaskStatus::Failed => self
                .store
                .update_status(task_id, TaskStatus::Pending)
                .map_err(|error| format!("将任务 {task_id} 回退到 Pending 失败: {error}")),
            // 终态：Completed / Killed 不可恢复；Running 仍在跑也不需要 resume
            other => Err(format!(
                "任务系统 不支持从 {:?} 状态恢复任务 {}",
                other, task_id
            )),
        }
    }

    fn apply_results(&self) -> Result<(), String> {
        for result in self.result_receiver.poll_results() {
            if !self.store.complete_lease(&result.task_id, &result.lease_id) {
                tracing::warn!(
                    task_id = %result.task_id,
                    lease_id = %result.lease_id,
                    "忽略非当前活跃租约的迟到任务结果"
                );
                continue;
            }
            match result.outcome {
                TaskOutcome::Completed { output_refs } => {
                    self.store.set_output_refs(&result.task_id, output_refs);
                    self.store
                        .update_status(&result.task_id, TaskStatus::Completed)
                        .map_err(|error| {
                            format!("任务 {} 完成状态写入失败: {error}", result.task_id)
                        })?;
                }
                TaskOutcome::Failed { error } => {
                    self.store.set_output_refs(&result.task_id, vec![error]);
                    self.store
                        .update_status(&result.task_id, TaskStatus::Failed)
                        .map_err(|err| {
                            format!("任务 {} 失败状态写入失败: {err}", result.task_id)
                        })?;
                }
            }
            self.set_checkpoint_signal();
        }
        Ok(())
    }

    fn expire_stale_leases(&self, root_task_id: &TaskId) -> Result<(), String> {
        for (task_id, lease_id) in self.store.collect_expired_leases(root_task_id) {
            self.store.revoke_lease(&task_id, &lease_id);
            if self.store.get_task(&task_id).is_some_and(|task| {
                matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
            }) {
                self.store
                    .update_status(&task_id, TaskStatus::Failed)
                    .map_err(|error| format!("任务 {task_id} 租约过期收口失败: {error}"))?;
                self.set_checkpoint_signal();
            }
        }
        Ok(())
    }

    fn match_worker(&self, task: &Task) -> Option<WorkerInfo> {
        let role = resolve_task_role(task, &self.agent_role_registry);
        self.workers
            .iter()
            .find(|worker| {
                worker.supported_kinds.contains(&task.kind)
                    && role.map(|role| worker.role == role).unwrap_or(true)
            })
            .cloned()
            .or_else(|| {
                self.workers
                    .iter()
                    .find(|worker| worker.supported_kinds.contains(&task.kind))
                    .cloned()
            })
    }

    fn terminal_state(&self, root_task_id: &TaskId) -> TerminalState {
        let task_ids = self.collect_subtree_ids(root_task_id);
        if task_ids.is_empty() {
            return TerminalState::NotTerminal;
        }
        let root_status = self.store.get_task(root_task_id).map(|task| task.status);
        let mut failed = Vec::new();
        let mut killed = Vec::new();
        let mut all_terminal = true;
        for task_id in task_ids {
            let Some(task) = self.store.get_task(&task_id) else {
                continue;
            };
            match task.status {
                TaskStatus::Completed => {}
                TaskStatus::Failed => failed.push(task_id),
                TaskStatus::Killed => killed.push(task_id),
                TaskStatus::Pending | TaskStatus::Running => all_terminal = false,
            }
        }
        if root_status == Some(TaskStatus::Completed) && all_terminal {
            TerminalState::AllCompleted
        } else if !failed.is_empty() && all_terminal {
            TerminalState::HasFailures(failed)
        } else if !killed.is_empty() && all_terminal {
            TerminalState::HasKilled(killed)
        } else if all_terminal {
            TerminalState::AllCompleted
        } else {
            TerminalState::NotTerminal
        }
    }

    fn collect_non_terminal_task_ids(&self, root_task_id: &TaskId) -> Vec<TaskId> {
        self.collect_subtree_ids(root_task_id)
            .into_iter()
            .filter(|task_id| {
                self.store.get_task(task_id).is_some_and(|task| {
                    matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                })
            })
            .collect()
    }

    fn collect_subtree_ids(&self, root_task_id: &TaskId) -> Vec<TaskId> {
        let mut ids = Vec::new();
        let mut stack = vec![root_task_id.clone()];
        let mut visited = HashSet::new();
        while let Some(task_id) = stack.pop() {
            if !visited.insert(task_id.clone()) {
                continue;
            }
            if self.store.get_task(&task_id).is_none() {
                continue;
            }
            for child in self.store.get_children(&task_id) {
                stack.push(child.task_id);
            }
            ids.push(task_id);
        }
        ids
    }
}

enum TerminalState {
    NotTerminal,
    AllCompleted,
    HasFailures(Vec<TaskId>),
    HasKilled(Vec<TaskId>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        execution_admission::{ExecutionAdmissionLimits, ExecutionAdmissionPermit},
        task_runner_bridge::{EventBasedResultReceiver, TaskResult},
    };
    use magi_core::{MissionId, TaskKind, TaskRuntimePayload, UtcMillis, WorkerId};
    use std::sync::Mutex;

    struct RejectingDispatcher;

    impl TaskDispatcher for RejectingDispatcher {
        fn dispatch(
            &self,
            _task: &Task,
            _worker: &WorkerInfo,
            _lease: &magi_orchestrator::task_store::TaskLease,
            _admission_permit: crate::execution_admission::ExecutionAdmissionPermit,
        ) -> Result<(), String> {
            Err("test dispatcher should not run".to_string())
        }
    }

    struct HoldingDispatcher {
        permits: Mutex<Vec<ExecutionAdmissionPermit>>,
    }

    impl HoldingDispatcher {
        fn release_all(&self) {
            self.permits
                .lock()
                .expect("held permits lock should not poison")
                .clear();
        }
    }

    impl TaskDispatcher for HoldingDispatcher {
        fn dispatch(
            &self,
            _task: &Task,
            _worker: &WorkerInfo,
            _lease: &magi_orchestrator::task_store::TaskLease,
            admission_permit: ExecutionAdmissionPermit,
        ) -> Result<(), String> {
            self.permits
                .lock()
                .expect("held permits lock should not poison")
                .push(admission_permit);
            Ok(())
        }
    }

    fn test_task(task_id: &str, root_task_id: &str, parent: Option<TaskId>) -> Task {
        let now = UtcMillis(1_000);
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new("mission-child-result"),
            root_task_id: TaskId::new(root_task_id),
            parent_task_id: parent,
            kind: TaskKind::LocalAgent,
            title: task_id.to_string(),
            goal: format!("run {task_id}"),
            status: TaskStatus::Pending,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn dispatch_gate_blocks_without_lease_or_status_change() {
        let store = Arc::new(TaskStore::new());
        let root = test_task("task-root-gated", "task-root-gated", None);
        store.insert_task(root.clone());
        let receiver = Arc::new(EventBasedResultReceiver::new());
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(RejectingDispatcher),
            receiver,
        )
        .with_dispatch_gate(Arc::new(|task: &Task| {
            Ok(TaskDispatchGateDecision::Blocked(format!(
                "blocked for {}",
                task.task_id
            )))
        }));

        let outcome = runner.run_cycle(&root.task_id);

        assert!(matches!(
            outcome,
            RunCycleOutcome::Blocked { ref task_ids, ref reason }
                if task_ids == &vec![root.task_id.clone()]
                    && reason.contains("blocked for task-root-gated")
        ));
        assert_eq!(
            store.get_task(&root.task_id).unwrap().status,
            TaskStatus::Pending
        );
        assert!(store.get_active_lease(&root.task_id).is_none());
    }

    #[test]
    fn stale_lease_result_cannot_overwrite_restarted_task() {
        let store = Arc::new(TaskStore::new());
        let mut root = test_task("task-stale-result", "task-stale-result", None);
        root.status = TaskStatus::Running;
        store.insert_task(root.clone());
        let worker_id = WorkerId::new("worker-stale-result");
        let stale_lease = store
            .grant_lease(
                &root.task_id,
                &root.root_task_id,
                &worker_id,
                "executor",
                DEFAULT_LEASE_DURATION_MS,
            )
            .expect("stale lease should grant");
        assert!(store.revoke_lease(&root.task_id, &stale_lease.lease_id));
        let current_lease = store
            .grant_lease(
                &root.task_id,
                &root.root_task_id,
                &worker_id,
                "executor",
                DEFAULT_LEASE_DURATION_MS,
            )
            .expect("current lease should grant");
        let receiver = Arc::new(EventBasedResultReceiver::new());
        receiver.push_result(TaskResult {
            task_id: root.task_id.clone(),
            lease_id: stale_lease.lease_id,
            outcome: TaskOutcome::Failed {
                error: "旧执行轮迟到失败".to_string(),
            },
        });
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(RejectingDispatcher),
            receiver,
        );

        assert_eq!(runner.run_cycle(&root.task_id), RunCycleOutcome::Continue);
        assert_eq!(
            store
                .get_task(&root.task_id)
                .expect("task should exist")
                .status,
            TaskStatus::Running
        );
        assert_eq!(
            store
                .get_active_lease(&root.task_id)
                .expect("current lease should remain")
                .lease_id,
            current_lease.lease_id
        );
    }

    #[test]
    fn shared_execution_admission_blocks_other_runners_until_the_running_task_finishes() {
        let store = Arc::new(TaskStore::new());
        let first_root = test_task("task-admission-first", "task-admission-first", None);
        let second_root = test_task("task-admission-second", "task-admission-second", None);
        store.insert_task(first_root.clone());
        store.insert_task(second_root.clone());
        let worker = WorkerInfo {
            worker_id: WorkerId::new("worker-admission-executor"),
            role: "executor".to_string(),
            supported_kinds: vec![TaskKind::LocalAgent],
            parallelism_limit: None,
            system_prompt_template: None,
        };
        let controller = Arc::new(
            crate::execution_admission::ExecutionAdmissionController::new(
                ExecutionAdmissionLimits {
                    max_active_tasks: 1,
                    max_active_tasks_per_session: 1,
                    max_active_tasks_per_role: 1,
                    min_available_memory_bytes: 0,
                },
            ),
        );
        let dispatcher = Arc::new(HoldingDispatcher {
            permits: Mutex::new(Vec::new()),
        });
        let first_runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            vec![worker.clone()],
            dispatcher.clone(),
            Arc::new(EventBasedResultReceiver::new()),
        )
        .with_execution_admission(
            Arc::clone(&controller),
            Some(SessionId::new("session-admission-first")),
        );
        let second_runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            vec![worker],
            dispatcher.clone(),
            Arc::new(EventBasedResultReceiver::new()),
        )
        .with_execution_admission(
            Arc::clone(&controller),
            Some(SessionId::new("session-admission-second")),
        );

        assert_eq!(
            first_runner.run_cycle(&first_root.task_id),
            RunCycleOutcome::Continue
        );
        let blocked = second_runner.run_cycle(&second_root.task_id);
        assert!(matches!(
            blocked,
            RunCycleOutcome::Blocked { ref task_ids, ref reason }
                if task_ids == &vec![second_root.task_id.clone()]
                    && reason.contains("全局执行容量已满")
        ));
        assert_eq!(
            store
                .get_task(&second_root.task_id)
                .expect("queued task should exist")
                .status,
            TaskStatus::Pending
        );
        assert!(store.get_active_lease(&second_root.task_id).is_none());
        assert_eq!(controller.snapshot().queued_task_count, 1);

        dispatcher.release_all();
        assert_eq!(
            second_runner.run_cycle(&second_root.task_id),
            RunCycleOutcome::Continue
        );
        assert_eq!(controller.snapshot().active_task_count, 1);
    }
}
