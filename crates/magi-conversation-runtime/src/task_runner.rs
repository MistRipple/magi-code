//! 任务系统 — TaskRunner 调度循环归属 conversation-runtime。
//!
//! 本模块只维护任务执行事实：pending/running/terminal 状态推进、
//! worker 匹配、租约、结果回收。

use crate::execution_admission::ExecutionAdmissionController;
#[cfg(test)]
use crate::task_runner_bridge::EventBasedResultReceiver;
use crate::task_runner_bridge::{
    RunCycleOutcome, TaskDispatchGate, TaskDispatchGateDecision, TaskDispatcher, TaskOutcome,
    TaskResult,
};
use magi_agent_role::AgentRoleRegistry;
use magi_core::{DomainError, SessionId, Task, TaskId, TaskStatus};
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
    time::Instant,
};

const DEFAULT_LEASE_DURATION_MS: u64 = 60_000;

pub struct TaskRunner {
    store: Arc<TaskStore>,
    /// 每次匹配都读取最新角色目录；已经派发的任务仍持有当次匹配复制出的
    /// WorkerInfo，因此角色编辑不会改变运行中的 Worker 快照。
    worker_catalog_provider: Arc<dyn Fn() -> Vec<WorkerInfo> + Send + Sync>,
    dispatcher: Arc<dyn TaskDispatcher>,
    #[cfg(test)]
    result_receiver: Option<Arc<EventBasedResultReceiver>>,
    dispatch_gate: Option<Arc<TaskDispatchGate>>,
    execution_admission: Arc<ExecutionAdmissionController>,
    session_id: Option<SessionId>,
    event_bus: Option<Arc<InMemoryEventBus>>,
    checkpoint_signal: AtomicBool,
    first_dispatch_reported: AtomicBool,
    agent_role_registry: AgentRoleRegistry,
}

/// 将 Worker 结果提交到 TaskStore。生产完成路径由
/// [`TaskCompletionNotifier`] 主动调用；测试可直接调用以验证完成合同。
pub fn apply_task_result(store: &TaskStore, result: TaskResult) -> Result<bool, String> {
    let Some(task) = store.get_task(&result.task_id) else {
        tracing::warn!(
            task_id = %result.task_id,
            lease_id = %result.lease_id,
            "忽略不存在任务的迟到结果"
        );
        return Ok(false);
    };
    let root_task_id = task.root_task_id.clone();
    match result.outcome {
        TaskOutcome::Completed { attempt } => match store.complete_lease_and_task(
            &result.task_id,
            &root_task_id,
            &result.lease_id,
            attempt,
        ) {
            Ok(true) => Ok(true),
            Ok(false) => {
                tracing::warn!(
                    task_id = %result.task_id,
                    lease_id = %result.lease_id,
                    "忽略非当前活跃租约的迟到任务结果"
                );
                Ok(false)
            }
            Err(DomainError::InvalidState { message }) => {
                let failure_message =
                    format!("任务 {} 完成合同验证失败: {message}", result.task_id);
                let changed = store
                    .revoke_lease_and_set_task_terminal(
                        &result.task_id,
                        &root_task_id,
                        Some(&result.lease_id),
                        TaskStatus::Failed,
                        vec![failure_message.clone()],
                    )
                    .map_err(|error| format!("{failure_message}；失败状态持久化失败: {error}"))?;
                if !changed {
                    return Err(format!(
                        "{failure_message}；当前任务租约已失效，未写入失败事实"
                    ));
                }
                Err(failure_message)
            }
            Err(error) => Err(format!(
                "任务 {} 完成事实持久化失败: {error}",
                result.task_id
            )),
        },
        TaskOutcome::Failed { error } => {
            let changed = store
                .revoke_lease_and_set_task_terminal(
                    &result.task_id,
                    &root_task_id,
                    Some(&result.lease_id),
                    TaskStatus::Failed,
                    vec![error],
                )
                .map_err(|err| format!("任务 {} 失败状态持久化失败: {err}", result.task_id))?;
            if !changed {
                tracing::warn!(
                    task_id = %result.task_id,
                    lease_id = %result.lease_id,
                    "忽略非当前活跃租约的迟到失败结果"
                );
            }
            Ok(changed)
        }
    }
}

impl TaskRunner {
    pub fn with_dispatcher_and_worker_catalog(
        store: Arc<TaskStore>,
        worker_catalog_provider: Arc<dyn Fn() -> Vec<WorkerInfo> + Send + Sync>,
        dispatcher: Arc<dyn TaskDispatcher>,
    ) -> Self {
        Self {
            store,
            worker_catalog_provider,
            dispatcher,
            #[cfg(test)]
            result_receiver: None,
            dispatch_gate: None,
            execution_admission: Arc::new(ExecutionAdmissionController::default()),
            session_id: None,
            event_bus: None,
            checkpoint_signal: AtomicBool::new(false),
            first_dispatch_reported: AtomicBool::new(false),
            agent_role_registry: AgentRoleRegistry::load_default(),
        }
    }

    #[cfg(test)]
    fn with_dispatcher(
        store: Arc<TaskStore>,
        workers: Vec<WorkerInfo>,
        dispatcher: Arc<dyn TaskDispatcher>,
    ) -> Self {
        Self::with_dispatcher_and_worker_catalog(
            store,
            Arc::new(move || workers.clone()),
            dispatcher,
        )
    }

    #[cfg(test)]
    pub fn with_test_result_receiver(
        store: Arc<TaskStore>,
        workers: Vec<WorkerInfo>,
        dispatcher: Arc<dyn TaskDispatcher>,
        result_receiver: Arc<EventBasedResultReceiver>,
    ) -> Self {
        let mut runner = Self::with_dispatcher(store, workers, dispatcher);
        runner.result_receiver = Some(result_receiver);
        runner
    }

    pub fn with_agent_role_registry(mut self, registry: AgentRoleRegistry) -> Self {
        self.agent_role_registry = registry;
        self
    }

    /// 注入动态 Worker 目录。目录只用于尚未派发任务的匹配，WorkerInfo 在成功匹配
    /// 后会被复制并随派发请求传递下去，确保角色热更新不改写运行中的任务。
    #[cfg(test)]
    fn with_worker_catalog_provider(
        mut self,
        provider: Arc<dyn Fn() -> Vec<WorkerInfo> + Send + Sync>,
    ) -> Self {
        self.worker_catalog_provider = provider;
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
        let cycle_started_at = Instant::now();
        #[cfg(test)]
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
            let task_ids = self.collect_non_terminal_task_ids(root_task_id);
            return if task_ids.is_empty() {
                RunCycleOutcome::Waiting
            } else {
                RunCycleOutcome::Unrunnable(task_ids)
            };
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
            let report_first_dispatch_timing =
                !self.first_dispatch_reported.load(Ordering::Relaxed);
            let lease = match self.store.grant_lease_and_start_task(
                &task.task_id,
                root_task_id,
                &worker.worker_id,
                &worker.role,
                DEFAULT_LEASE_DURATION_MS,
            ) {
                Ok(Some(lease)) => lease,
                Ok(None) => {
                    drop(admission_permit);
                    continue;
                }
                Err(error) => {
                    drop(admission_permit);
                    return RunCycleOutcome::Error(format!(
                        "任务 {} 获取执行租约并进入 running 失败: {error}",
                        task.task_id
                    ));
                }
            };
            if report_first_dispatch_timing {
                tracing::info!(
                    target: "magi.performance",
                    root_task_id = %root_task_id,
                    task_id = %task.task_id,
                    worker_id = %worker.worker_id,
                    elapsed_ms = cycle_started_at.elapsed().as_millis() as u64,
                    stage = "task_runner_lease_granted",
                    "conversation response timing"
                );
            }
            if let Err(error) = self
                .dispatcher
                .dispatch(&task, &worker, &lease, admission_permit)
            {
                let failure_message = format!("任务 {} 派发失败: {error}", task.task_id);
                match self.store.revoke_lease_and_set_task_terminal(
                    &task.task_id,
                    root_task_id,
                    Some(&lease.lease_id),
                    TaskStatus::Failed,
                    vec![failure_message.clone()],
                ) {
                    Ok(true) => {}
                    Ok(false) => {
                        return RunCycleOutcome::Error(format!(
                            "{failure_message}；当前租约已失效，未写入失败事实"
                        ));
                    }
                    Err(status_error) => {
                        return RunCycleOutcome::Error(format!(
                            "{failure_message}；失败事实持久化失败: {status_error}"
                        ));
                    }
                }
                self.set_checkpoint_signal();
                return RunCycleOutcome::Error(failure_message);
            }
            if !self.first_dispatch_reported.swap(true, Ordering::Relaxed) {
                tracing::info!(
                    target: "magi.performance",
                    root_task_id = %root_task_id,
                    task_id = %task.task_id,
                    worker_id = %worker.worker_id,
                    elapsed_ms = cycle_started_at.elapsed().as_millis() as u64,
                    stage = "task_runner_first_dispatch_completed",
                    "conversation response timing"
                );
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
        } else if unmatched.is_empty() {
            RunCycleOutcome::Waiting
        } else {
            RunCycleOutcome::Unrunnable(unmatched)
        }
    }

    pub fn finalize_unrunnable_outcome(
        &self,
        root_task_id: &TaskId,
        task_ids: &[TaskId],
    ) -> Result<(), String> {
        for task_id in task_ids {
            if let Some(task) = self.store.get_task(task_id)
                && matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
            {
                self.close_task(
                    root_task_id,
                    task_id,
                    TaskStatus::Failed,
                    vec![self.unrunnable_task_reason(&task)],
                )?;
            }
        }
        // A root can remain Pending when only an unmatched leaf was supplied in
        // `task_ids`.  Close it in the same terminal transition so the Task
        // fact and the Turn completion notifier cannot be left waiting for a
        // Runner observer that no longer exists.
        if let Some(root_task) = self.store.get_task(root_task_id)
            && matches!(root_task.status, TaskStatus::Pending | TaskStatus::Running)
        {
            self.close_task(
                root_task_id,
                root_task_id,
                TaskStatus::Failed,
                vec![self.unrunnable_task_reason(&root_task)],
            )?;
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
                self.close_task(
                    root_task_id,
                    &task_id,
                    TaskStatus::Failed,
                    vec![reason.to_string()],
                )?;
            }
        }
        self.set_checkpoint_signal();
        Ok(())
    }

    fn unrunnable_task_reason(&self, task: &Task) -> String {
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
        let changed =
            self.close_task(&task.root_task_id, task_id, TaskStatus::Killed, Vec::new())?;
        if !changed {
            return Err(format!("终止任务 {task_id} 时当前任务租约已失效"));
        }
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
            // 用户显式继续：把 Failed 任务重新打开为 Pending，由 dispatcher 重新派发
            // 这条路径是 `/api/session/continue` 的 root-status==Failed 入口
            TaskStatus::Failed => self
                .store
                .reopen_failed_task_for_recovery(task_id)
                .map_err(|error| format!("将任务 {task_id} 重新打开为 Pending 失败: {error}")),
            // 终态：Completed / Killed 不可恢复；Running 仍在跑也不需要 resume
            other => Err(format!(
                "任务系统 不支持从 {:?} 状态恢复任务 {}",
                other, task_id
            )),
        }
    }

    #[cfg(test)]
    fn apply_results(&self) -> Result<(), String> {
        let Some(result_receiver) = self.result_receiver.as_ref() else {
            return Ok(());
        };
        for result in result_receiver.poll_results_for_test() {
            let _ = apply_task_result(&self.store, result)?;
            self.set_checkpoint_signal();
        }
        Ok(())
    }

    fn expire_stale_leases(&self, root_task_id: &TaskId) -> Result<(), String> {
        for (task_id, lease_id) in self.store.collect_expired_leases(root_task_id) {
            if self.store.get_task(&task_id).is_some_and(|task| {
                matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
            }) {
                let changed = self
                    .store
                    .revoke_lease_and_set_task_terminal(
                        &task_id,
                        root_task_id,
                        Some(&lease_id),
                        TaskStatus::Failed,
                        vec![format!("任务 {task_id} 租约已过期")],
                    )
                    .map_err(|error| format!("任务 {task_id} 租约过期收口失败: {error}"))?;
                if !changed {
                    continue;
                }
                self.set_checkpoint_signal();
            }
        }
        Ok(())
    }

    fn close_task(
        &self,
        root_task_id: &TaskId,
        task_id: &TaskId,
        status: TaskStatus,
        output_refs: Vec<String>,
    ) -> Result<bool, String> {
        let lease_id = self
            .store
            .get_active_lease(task_id)
            .map(|lease| lease.lease_id);
        self.store
            .revoke_lease_and_set_task_terminal(
                task_id,
                root_task_id,
                lease_id.as_ref(),
                status,
                output_refs,
            )
            .map_err(|error| format!("任务 {task_id} 收口失败: {error}"))
    }

    fn match_worker(&self, task: &Task) -> Option<WorkerInfo> {
        let explicitly_bound_role = task.executor_binding_target_role();
        let role = resolve_task_role(task, &self.agent_role_registry);
        // 显式角色一旦无法在当前 registry 中解析，必须保持不可运行状态；
        // 不能把用户明确要求的角色静默改派给 executor 或其他 worker。
        if explicitly_bound_role.is_some() && role.is_none() {
            return None;
        }
        let workers = (self.worker_catalog_provider)();
        workers
            .iter()
            .find(|worker| {
                worker.supported_kinds.contains(&task.kind)
                    && role.map(|role| worker.role == role).unwrap_or(true)
            })
            .cloned()
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
    use magi_core::{
        DomainError, MissionId, TaskCompletionAttempt, TaskKind, TaskRuntimePayload, UtcMillis,
        WorkerId,
    };
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
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
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
        store.insert_task(root.clone()).expect("根任务应插入");
        let receiver = Arc::new(EventBasedResultReceiver::new());
        let runner = TaskRunner::with_test_result_receiver(
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
    fn unmatched_task_is_reported_as_unrunnable() {
        let store = Arc::new(TaskStore::new());
        let root = test_task("task-root-unmatched", "task-root-unmatched", None);
        store.insert_task(root.clone()).expect("根任务应插入");
        let runner = TaskRunner::with_test_result_receiver(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(RejectingDispatcher),
            Arc::new(EventBasedResultReceiver::new()),
        );

        assert_eq!(
            runner.run_cycle(&root.task_id),
            RunCycleOutcome::Unrunnable(vec![root.task_id.clone()])
        );
        assert_eq!(
            store
                .get_task(&root.task_id)
                .expect("task should remain available for failure reporting")
                .status,
            TaskStatus::Pending
        );
    }

    #[test]
    fn explicit_unknown_role_is_not_silently_reassigned_to_executor() {
        let store = Arc::new(TaskStore::new());
        let mut root = test_task(
            "task-explicit-unknown-role",
            "task-explicit-unknown-role",
            None,
        );
        root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("missing-role"));
        store.insert_task(root.clone()).expect("根任务应插入");
        let executor = WorkerInfo {
            worker_id: WorkerId::new("worker-executor-reassignment-check"),
            role: "executor".to_string(),
            supported_kinds: vec![TaskKind::LocalAgent],
            parallelism_limit: None,
            system_prompt_template: None,
        };
        let runner = TaskRunner::with_test_result_receiver(
            Arc::clone(&store),
            vec![executor],
            Arc::new(RejectingDispatcher),
            Arc::new(EventBasedResultReceiver::new()),
        );

        assert_eq!(
            runner.run_cycle(&root.task_id),
            RunCycleOutcome::Unrunnable(vec![root.task_id.clone()])
        );
        assert_eq!(
            store
                .get_task(&root.task_id)
                .expect("task should remain pending")
                .status,
            TaskStatus::Pending
        );
    }

    #[test]
    fn dynamic_worker_catalog_provider_sees_role_added_after_runner_creation() {
        let store = Arc::new(TaskStore::new());
        let mut root = test_task("task-dynamic-role", "task-dynamic-role", None);
        root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("dynamic-role"));
        store.insert_task(root.clone()).expect("任务应插入");

        let role_dir = tempfile::tempdir().expect("角色目录应创建");
        let registry = AgentRoleRegistry::builtin().with_user_role_dir(role_dir.path());
        let provider_registry = registry.clone();
        let provider = Arc::new(move || {
            let role_ids = provider_registry.spawnable_agent_role_ids();
            magi_orchestrator::task_worker_catalog::build_worker_catalog_for_roles(
                &provider_registry,
                role_ids,
            )
        });
        let dispatcher = Arc::new(HoldingDispatcher {
            permits: Mutex::new(Vec::new()),
        });
        let runner = TaskRunner::with_test_result_receiver(
            Arc::clone(&store),
            Vec::new(),
            dispatcher,
            Arc::new(EventBasedResultReceiver::new()),
        )
        .with_agent_role_registry(registry.clone())
        .with_worker_catalog_provider(provider);

        let mut role = registry.get("executor").expect("内置 executor 应存在");
        role.id = "dynamic-role".to_string();
        role.display_name = "动态角色".to_string();
        role.role = "动态角色".to_string();
        role.system_prompt = "你是动态加载的角色。".to_string();
        registry
            .save_user_role(role, None)
            .expect("运行中新增角色应保存成功");

        assert_eq!(
            runner.run_cycle(&root.task_id),
            RunCycleOutcome::Continue,
            "Runner 创建后新增的用户角色应能匹配后续任务"
        );
        let lease = store
            .get_active_lease(&root.task_id)
            .expect("任务应获得动态角色的活跃租约");
        assert_eq!(lease.role, "dynamic-role");
        assert_eq!(lease.worker_id.as_str(), "task-worker-dynamic-role");
    }

    #[test]
    fn stale_lease_result_cannot_overwrite_restarted_task() {
        let store = Arc::new(TaskStore::new());
        let mut root = test_task("task-stale-result", "task-stale-result", None);
        root.status = TaskStatus::Running;
        store.insert_task(root.clone()).expect("根任务应插入");
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
        let runner = TaskRunner::with_test_result_receiver(
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
    fn completion_persistence_error_is_reported_without_probing_task_memory_state() {
        let store = Arc::new(TaskStore::new());
        let mut root = test_task(
            "task-completion-persistence",
            "task-completion-persistence",
            None,
        );
        root.status = TaskStatus::Running;
        store.insert_task(root.clone()).expect("root should insert");
        let lease = store
            .grant_lease(
                &root.task_id,
                &root.root_task_id,
                &WorkerId::new("worker-completion-persistence"),
                "executor",
                DEFAULT_LEASE_DURATION_MS,
            )
            .expect("lease should grant");
        store.set_checkpoint_callback(Box::new(|_| {
            Err(DomainError::Persistence {
                message: "completion checkpoint unavailable".to_string(),
            })
        }));

        let receiver = Arc::new(EventBasedResultReceiver::new());
        receiver.push_result(TaskResult {
            task_id: root.task_id.clone(),
            lease_id: lease.lease_id.clone(),
            outcome: TaskOutcome::Completed {
                attempt: TaskCompletionAttempt {
                    output_refs: vec!["output://completion".to_string()],
                    final_response: Some("完成".to_string()),
                    evidence: Vec::new(),
                },
            },
        });
        let runner = TaskRunner::with_test_result_receiver(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(RejectingDispatcher),
            receiver,
        );

        let outcome = runner.run_cycle(&root.task_id);
        assert!(matches!(
            outcome,
            RunCycleOutcome::Error(ref reason)
                if reason.contains("完成事实持久化失败")
                    && reason.contains("completion checkpoint unavailable")
        ));
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
                .expect("lease should remain active")
                .lease_id,
            lease.lease_id
        );
    }

    #[test]
    fn shared_execution_admission_blocks_other_runners_until_the_running_task_finishes() {
        let store = Arc::new(TaskStore::new());
        let first_root = test_task("task-admission-first", "task-admission-first", None);
        let second_root = test_task("task-admission-second", "task-admission-second", None);
        store
            .insert_task(first_root.clone())
            .expect("第一个根任务应插入");
        store
            .insert_task(second_root.clone())
            .expect("第二个根任务应插入");
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
        let first_runner = TaskRunner::with_test_result_receiver(
            Arc::clone(&store),
            vec![worker.clone()],
            dispatcher.clone(),
            Arc::new(EventBasedResultReceiver::new()),
        )
        .with_execution_admission(
            Arc::clone(&controller),
            Some(SessionId::new("session-admission-first")),
        );
        let second_runner = TaskRunner::with_test_result_receiver(
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
