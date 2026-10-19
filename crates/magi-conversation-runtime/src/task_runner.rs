//! 任务系统 — TaskRunner 调度循环归属 conversation-runtime。
//!
//! 本模块只维护任务执行事实：pending/running/terminal 状态推进、
//! worker 匹配、租约、结果回收。

use crate::execution_admission::ExecutionAdmissionController;
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
                    "忽略租约或父执行链已失效的迟到任务结果"
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
        TaskOutcome::Cancelled => store
            .revoke_lease_and_set_task_terminal(
                &result.task_id,
                &root_task_id,
                Some(&result.lease_id),
                TaskStatus::Killed,
                Vec::new(),
            )
            .map_err(|error| format!("任务 {} 取消状态持久化失败: {error}", result.task_id)),
        TaskOutcome::Yielded => {
            // 等待事实已由 TaskStore 原子提交；这份回执不再提交或通知任何状态。
            Ok(false)
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
        if let Err(error) = self.expire_stale_leases(root_task_id) {
            return RunCycleOutcome::Error(error);
        }
        if let Err(error) = self.kill_descendants_of_terminal_tasks(root_task_id) {
            return RunCycleOutcome::Error(error);
        }

        let active_leases = self.store.collect_active_leases(root_task_id);
        for (task_id, lease_id) in &active_leases {
            self.store
                .heartbeat_lease_for_execution_scope(root_task_id, task_id, lease_id);
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
            return if self.store.has_pending_user_input(root_task_id) {
                RunCycleOutcome::UserInputRequired
            } else if task_ids.is_empty() {
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
            let lease = match self.store.claim_task_for_execution_scope(
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
            // 排队补充与 grant 共用 TaskStore mutation 锁；grant 之后重新读取，
            // 不可把准入扫描时的旧任务快照交给模型。
            let Some(task) = self.store.get_task(&task.task_id) else {
                return RunCycleOutcome::Error("已获执行租约的任务记录缺失".into());
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
                    &task.root_task_id,
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
            if self.store.has_pending_user_input(root_task_id) {
                RunCycleOutcome::UserInputRequired
            } else {
                RunCycleOutcome::Waiting
            }
        } else {
            RunCycleOutcome::Unrunnable(unmatched)
        }
    }

    /// 扫描不是终态授权；返回 false 表示候选已失效，调用方必须继续观察，不能结束 Runner。
    pub fn finalize_unrunnable_outcome(
        &self,
        root_task_id: &TaskId,
        task_ids: &[TaskId],
    ) -> Result<bool, String> {
        let mut reasons = task_ids
            .iter()
            .filter_map(|task_id| self.store.get_task(task_id))
            .filter(|task| matches!(task.status, TaskStatus::Pending | TaskStatus::Running))
            .map(|task| self.unrunnable_task_reason(&task))
            .collect::<Vec<_>>();
        if reasons.is_empty() {
            return Ok(false);
        }
        reasons.sort();
        reasons.dedup();
        let result = self
            .store
            .fail_unrunnable_execution_scope(root_task_id, task_ids, reasons)
            .map_err(|error| error.to_string());
        release_terminal_admissions(
            &self.store,
            &self.execution_admission,
            self.store.collect_execution_subtree_ids(root_task_id),
        );
        let changed = result?;
        if changed {
            self.set_checkpoint_signal();
        }
        Ok(changed)
    }

    pub fn finalize_unexpected_failure(
        &self,
        root_task_id: &TaskId,
        reason: &str,
    ) -> Result<(), String> {
        settle_subtree_in_store(
            &self.store,
            &self.execution_admission,
            root_task_id,
            TaskStatus::Failed,
            vec![reason.to_string()],
            true,
        )?;
        self.set_checkpoint_signal();
        Ok(())
    }

    fn unrunnable_task_reason(&self, task: &Task) -> String {
        let role = resolve_task_role(task, &self.agent_role_registry)
            .or_else(|| task.executor_binding_target_role())
            .unwrap_or("unknown");
        if task.parent_task_id.is_some() {
            return format!("代理不可用：没有匹配角色 {role} 的可用执行器。");
        }
        format!("任务不可运行：没有匹配角色 {role} 的可用执行器。")
    }

    pub fn kill_task(&self, task_id: &TaskId) -> Result<(), String> {
        if kill_task_in_store(&self.store, &self.execution_admission, task_id, Vec::new())? {
            self.set_checkpoint_signal();
        }
        Ok(())
    }

    /// 尽力终止整棵任务树：单个任务收口失败不会阻止其余任务被终止，
    /// 所有失败在全部尝试之后合并返回。
    pub fn kill_tree(&self, root_task_id: &TaskId) -> Result<(), String> {
        let changed = kill_subtree_in_store(
            &self.store,
            &self.execution_admission,
            root_task_id,
            Vec::new(),
        )?;
        if changed {
            self.set_checkpoint_signal();
        }
        Ok(())
    }

    /// 普通停止只终止当前拥有方的执行范围；完整会话销毁仍使用 kill_tree。
    pub fn kill_execution_scope(&self, scope_root: &TaskId) -> Result<(), String> {
        if settle_subtree_in_store(
            &self.store,
            &self.execution_admission,
            scope_root,
            TaskStatus::Killed,
            Vec::new(),
            true,
        )? {
            self.set_checkpoint_signal();
        }
        Ok(())
    }

    /// 已结束任务下仍未结束的后代没有人会再收集结果，统一级联终止。
    ///
    /// 根任务失败、被终止或在子代理仍运行时完成，都在 TaskStore 同一快照下收口
    /// 当前执行范围，不能依赖逐项扫描再取消，也不能跨已提交的转交边界。
    fn kill_descendants_of_terminal_tasks(&self, root_task_id: &TaskId) -> Result<(), String> {
        let result = self
            .store
            .revoke_orphaned_execution_descendants(root_task_id)
            .map_err(|error| error.to_string());
        release_terminal_admissions(
            &self.store,
            &self.execution_admission,
            self.store.collect_execution_subtree_ids(root_task_id),
        );
        if result? {
            self.set_checkpoint_signal();
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

    fn expire_stale_leases(&self, root_task_id: &TaskId) -> Result<(), String> {
        for (task_id, lease_id) in self.store.collect_expired_leases(root_task_id) {
            if self.store.get_task(&task_id).is_some_and(|task| {
                matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
            }) {
                let changed = self
                    .store
                    .expire_lease_for_execution_scope(root_task_id, &task_id, &lease_id)
                    .map_err(|error| format!("任务 {task_id} 租约过期收口失败: {error}"))?;
                if !changed {
                    continue;
                }
                self.set_checkpoint_signal();
            }
        }
        Ok(())
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
        let task_ids = self.store.collect_execution_subtree_ids(root_task_id);
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
        self.store
            .collect_execution_subtree_ids(root_task_id)
            .into_iter()
            .filter(|task_id| {
                self.store.get_task(task_id).is_some_and(|task| {
                    matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                })
            })
            .collect()
    }
}

enum TerminalState {
    NotTerminal,
    AllCompleted,
    HasFailures(Vec<TaskId>),
    HasKilled(Vec<TaskId>),
}

/// 终止单个任务：撤销其租约、写入 Killed 终态并移出准入队列。
///
/// 返回是否由本次调用改变了任务状态。任务在读取与收口之间自行结束属于正常竞态，
/// 只要它已处于终态就视为终止目标达成。
pub(crate) fn kill_task_in_store(
    store: &TaskStore,
    admission: &ExecutionAdmissionController,
    task_id: &TaskId,
    output_refs: Vec<String>,
) -> Result<bool, String> {
    let task = store
        .get_task(task_id)
        .ok_or_else(|| format!("任务不存在: {task_id}"))?;
    if task_status_is_finished(task.status) {
        admission.release_pending(task_id);
        return Ok(false);
    }
    let changed = store
        .revoke_active_lease_and_set_task_terminal(
            task_id,
            &task.root_task_id,
            TaskStatus::Killed,
            output_refs,
        )
        .map_err(|error| format!("任务 {task_id} 取消收口失败: {error}"))?;
    if !changed
        && !store
            .get_task(task_id)
            .is_some_and(|current| task_status_is_finished(current.status))
    {
        return Err(format!("终止任务 {task_id} 时当前任务租约已失效"));
    }
    admission.release_pending(task_id);
    Ok(changed)
}

/// 尽力终止整棵子树：单个任务收口失败不会阻止其余任务被终止，所有失败在全部
/// 尝试之后合并返回。返回是否有任务状态被本次调用改变。
pub(crate) fn kill_subtree_in_store(
    store: &TaskStore,
    admission: &ExecutionAdmissionController,
    root_task_id: &TaskId,
    output_refs: Vec<String>,
) -> Result<bool, String> {
    settle_subtree_in_store(
        store,
        admission,
        root_task_id,
        TaskStatus::Killed,
        output_refs,
        false,
    )
}

pub(crate) fn settle_subtree_in_store(
    store: &TaskStore,
    admission: &ExecutionAdmissionController,
    root_task_id: &TaskId,
    status: TaskStatus,
    output_refs: Vec<String>,
    execution_only: bool,
) -> Result<bool, String> {
    let result = if execution_only {
        store.revoke_active_leases_and_set_execution_subtree_terminal(
            root_task_id,
            status,
            output_refs,
        )
    } else {
        store.revoke_active_leases_and_set_subtree_terminal(root_task_id, status, output_refs)
    }
    .map_err(|error| error.to_string());
    // 仅释放已提交终态的预占/排队；checkpoint 失败或坏节点保留原资格，
    // 不得把未持久化的取消当作已经完成。
    let ids = if execution_only {
        store.collect_execution_subtree_ids(root_task_id)
    } else {
        store.collect_subtree_ids(root_task_id)
    };
    release_terminal_admissions(store, admission, ids);
    result
}

fn release_terminal_admissions(
    store: &TaskStore,
    admission: &ExecutionAdmissionController,
    ids: Vec<TaskId>,
) {
    for task_id in ids {
        if store
            .get_task(&task_id)
            .is_some_and(|task| task_status_is_finished(task.status))
        {
            admission.release_pending(&task_id);
        }
    }
}

fn task_status_is_finished(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        execution_admission::{ExecutionAdmissionLimits, ExecutionAdmissionPermit},
        task_execution_registry::TaskExecutionRegistry,
        task_runner_bridge::TaskResult,
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
    fn subtree_terminal_failure_keeps_entire_subtree_and_pending_admission() {
        for mode in ["kill", "execution_kill", "unexpected_failure", "unrunnable"] {
            let store = Arc::new(TaskStore::new());
            let mut root = test_task("task-root-kill", "task-root-kill", None);
            root.status = TaskStatus::Running;
            let child = test_task(
                "task-child-kill",
                "task-root-kill",
                Some(root.task_id.clone()),
            );
            let grandchild = test_task(
                "task-leaf-kill",
                "task-root-kill",
                Some(child.task_id.clone()),
            );
            for task in [root.clone(), child.clone(), grandchild.clone()] {
                store.insert_task(task).unwrap();
            }
            let admission = Arc::new(ExecutionAdmissionController::new(
                ExecutionAdmissionLimits {
                    max_active_tasks: 1,
                    max_active_tasks_per_session: 1,
                    max_active_tasks_per_role: 1,
                    min_available_memory_bytes: 0,
                },
            ));
            assert!(
                admission
                    .reserve(root.task_id.clone(), None, "executor")
                    .is_none()
            );
            assert!(
                admission
                    .reserve(child.task_id.clone(), None, "executor")
                    .is_some()
            );
            let runner = TaskRunner::with_dispatcher(
                Arc::clone(&store),
                Vec::new(),
                Arc::new(RejectingDispatcher),
            )
            .with_execution_admission(admission.clone(), None);
            let before = serde_json::to_value(store.snapshot().tasks).unwrap();
            store.set_checkpoint_callback(Box::new(|_| {
                Err(DomainError::InvalidState {
                    message: "disk failed".into(),
                })
            }));
            let settle = || match mode {
                "kill" => runner.kill_tree(&root.task_id),
                "execution_kill" => runner.kill_execution_scope(&root.task_id),
                "unexpected_failure" => {
                    runner.finalize_unexpected_failure(&root.task_id, "worker failed")
                }
                _ => runner
                    .finalize_unrunnable_outcome(
                        &root.task_id,
                        std::slice::from_ref(&child.task_id),
                    )
                    .map(|_| ()),
            };
            assert!(settle().is_err());
            assert_eq!(
                serde_json::to_value(store.snapshot().tasks).unwrap(),
                before
            );
            assert_eq!(admission.snapshot().active_task_count, 1);
            assert_eq!(admission.snapshot().queued_task_count, 1);
            store.set_checkpoint_callback(Box::new(|_| Ok(())));
            settle().unwrap();
            for task in [root, child, grandchild] {
                assert_eq!(
                    store.get_task(&task.task_id).unwrap().status,
                    if matches!(mode, "kill" | "execution_kill") {
                        TaskStatus::Killed
                    } else {
                        TaskStatus::Failed
                    }
                );
            }
            assert_eq!(admission.snapshot().active_task_count, 0);
            assert_eq!(admission.snapshot().queued_task_count, 0);
        }
    }

    #[test]
    fn failed_root_kills_running_and_queued_children_instead_of_dispatching_them() {
        let store = Arc::new(TaskStore::new());
        let mut root = test_task("task-root-failed", "task-root-failed", None);
        root.status = TaskStatus::Failed;
        let mut running_child = test_task(
            "task-running-orphan",
            "task-root-failed",
            Some(root.task_id.clone()),
        );
        running_child.status = TaskStatus::Running;
        let queued_child = test_task(
            "task-queued-orphan",
            "task-root-failed",
            Some(root.task_id.clone()),
        );
        for task in [root.clone(), running_child.clone(), queued_child.clone()] {
            store.insert_task(task).expect("任务应插入");
        }
        // 父任务已结束时排队中的子代理不能再被派发；RejectingDispatcher 被调用即报错。
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            vec![WorkerInfo {
                worker_id: WorkerId::new("worker-orphan"),
                role: "executor".to_string(),
                supported_kinds: vec![TaskKind::LocalAgent],
                parallelism_limit: None,
                system_prompt_template: None,
            }],
            Arc::new(RejectingDispatcher),
        );

        let outcome = runner.run_cycle(&root.task_id);

        assert!(
            matches!(outcome, RunCycleOutcome::Error(ref error) if error.contains("任务执行失败")),
            "根任务失败后应直接收口：{outcome:?}"
        );
        for task_id in [&running_child.task_id, &queued_child.task_id] {
            assert_eq!(
                store.get_task(task_id).expect("任务应存在").status,
                TaskStatus::Killed,
                "{task_id} 必须随失败的根任务一起终止"
            );
        }
    }

    #[test]
    fn transferred_subtree_has_its_own_runner_after_source_turn_task_finishes() {
        for terminal in [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Killed,
        ] {
            let store = Arc::new(TaskStore::new());
            let mut root = test_task("scope-root", "scope-root", None);
            root.status = TaskStatus::Running;
            root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
            store.insert_task(root.clone()).unwrap();
            let mut child = test_task("scope-child", "scope-root", Some(root.task_id.clone()));
            let mut binding = magi_core::TaskExecutorBinding::for_role("executor");
            binding.delegation_allowed = true;
            binding.agent_activation = Some(magi_core::AgentActivation {
                agent_id: child.task_id.clone(),
                session_id: magi_core::SessionId::new("scope-session"),
                lifetime: magi_core::AgentLifetime::Session,
                generation: 1,
                previous_task_id: None,
            });
            child.executor_binding = Some(binding.clone());
            let mut leaf = test_task("scope-leaf", "scope-root", Some(child.task_id.clone()));
            binding.delegation_allowed = false;
            binding.agent_activation.as_mut().unwrap().agent_id = leaf.task_id.clone();
            leaf.executor_binding = Some(binding);
            for task in [&child, &leaf] {
                store.insert_task(task.clone()).unwrap();
            }
            store
                .transfer_agent_subtree(
                    &root.task_id,
                    &child.task_id,
                    magi_core::AgentRunTransfer {
                        operation_id: "runner-transfer".into(),
                        session_id: magi_core::SessionId::new("scope-session"),
                        source_turn_id: "source-turn".into(),
                        transferred_by: root.task_id.clone(),
                        activation_generation: 1,
                        owner: magi_core::AgentRunOwner::Session,
                        transferred_at: UtcMillis::now(),
                    },
                )
                .unwrap();
            if terminal == TaskStatus::Completed {
                store
                    .complete_task(
                        &root.task_id,
                        TaskCompletionAttempt {
                            output_refs: vec![],
                            final_response: Some("主线结束".into()),
                            evidence: vec![],
                        },
                    )
                    .unwrap();
            } else {
                store
                    .revoke_active_lease_and_set_task_terminal(
                        &root.task_id,
                        &root.task_id,
                        terminal,
                        vec![],
                    )
                    .unwrap();
            }
            let dispatcher = Arc::new(HoldingDispatcher {
                permits: Mutex::new(vec![]),
            });
            let runner = TaskRunner::with_dispatcher(
                store.clone(),
                vec![WorkerInfo {
                    worker_id: WorkerId::new("scope-worker"),
                    role: "executor".into(),
                    supported_kinds: vec![TaskKind::LocalAgent],
                    parallelism_limit: None,
                    system_prompt_template: None,
                }],
                dispatcher.clone(),
            );
            // 原 Runner 可收口，不得级联杀死或等待已转交子树。
            let source = runner.run_cycle(&root.task_id);
            assert!(
                match terminal {
                    TaskStatus::Completed => source == RunCycleOutcome::AllComplete,
                    _ => matches!(source, RunCycleOutcome::Error(_)),
                },
                "{source:?}"
            );
            assert_eq!(
                store.get_task(&child.task_id).unwrap().status,
                TaskStatus::Pending
            );
            assert_eq!(runner.run_cycle(&child.task_id), RunCycleOutcome::Continue);
            let child_lease = store.get_active_lease(&child.task_id).unwrap();
            assert_eq!(child_lease.root_task_id, root.task_id);
            assert!(
                !crate::task_execution_guard::TaskExecutionGuard::new(
                    &store,
                    &magi_session_store::SessionStore::new(),
                    &TaskExecutionRegistry::default(),
                    &magi_core::SessionId::new("scope-session"),
                    &child.task_id,
                    &child_lease.lease_id,
                )
                .is_active(),
                "没有 canonical 拥有方装配时，领域转交不能单独授予执行权"
            );
            assert_eq!(runner.run_cycle(&child.task_id), RunCycleOutcome::Continue);
            let leaf_lease = store.get_active_lease(&leaf.task_id).unwrap();
            for (task, lease) in [(&leaf, &leaf_lease), (&child, &child_lease)] {
                assert!(
                    store
                        .complete_lease_and_task(
                            &task.task_id,
                            &root.task_id,
                            &lease.lease_id,
                            TaskCompletionAttempt {
                                output_refs: vec![],
                                final_response: Some("独立拥有方完成".into()),
                                evidence: vec![]
                            }
                        )
                        .unwrap()
                );
            }
            assert_eq!(
                runner.run_cycle(&child.task_id),
                RunCycleOutcome::AllComplete
            );
            assert_eq!(store.get_task(&root.task_id).unwrap().status, terminal);
            dispatcher.release_all();
        }
    }

    #[test]
    fn old_runner_scan_cannot_claim_a_subtree_transferred_during_admission() {
        let store = Arc::new(TaskStore::new());
        let mut root = test_task("claim-root", "claim-root", None);
        root.status = TaskStatus::Running;
        root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
        store.insert_task(root.clone()).unwrap();
        let mut child = test_task("claim-child", "claim-root", Some(root.task_id.clone()));
        let mut binding = magi_core::TaskExecutorBinding::for_role("executor");
        binding.agent_activation = Some(magi_core::AgentActivation {
            agent_id: child.task_id.clone(),
            session_id: magi_core::SessionId::new("claim-session"),
            lifetime: magi_core::AgentLifetime::Session,
            generation: 1,
            previous_task_id: None,
        });
        child.executor_binding = Some(binding);
        store.insert_task(child.clone()).unwrap();
        let runtime = store.clone();
        let parent = root.clone();
        let runner = TaskRunner::with_dispatcher(
            store.clone(),
            vec![WorkerInfo {
                worker_id: WorkerId::new("claim-worker"),
                role: "executor".into(),
                supported_kinds: vec![TaskKind::LocalAgent],
                parallelism_limit: None,
                system_prompt_template: None,
            }],
            Arc::new(RejectingDispatcher),
        )
        .with_dispatch_gate(Arc::new(move |task| {
            runtime
                .transfer_agent_subtree(
                    &parent.task_id,
                    &task.task_id,
                    magi_core::AgentRunTransfer {
                        operation_id: "during-admission".into(),
                        session_id: magi_core::SessionId::new("claim-session"),
                        source_turn_id: "source-turn".into(),
                        transferred_by: parent.task_id.clone(),
                        activation_generation: 1,
                        owner: magi_core::AgentRunOwner::Session,
                        transferred_at: UtcMillis::now(),
                    },
                )
                .unwrap();
            Ok(TaskDispatchGateDecision::Allow)
        }));
        assert_eq!(runner.run_cycle(&root.task_id), RunCycleOutcome::Waiting);
        assert_eq!(
            store.get_task(&child.task_id).unwrap().status,
            TaskStatus::Pending
        );
        assert!(store.get_active_lease(&child.task_id).is_none());
    }

    #[test]
    fn stale_unrunnable_child_does_not_fail_mainline_or_release_transferred_admission() {
        for changed in ["transferred", "completed", "started", "active"] {
            let store = Arc::new(TaskStore::new());
            let mut root = test_task("unrunnable-root", "unrunnable-root", None);
            root.status = TaskStatus::Running;
            root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
            store.insert_task(root.clone()).unwrap();
            let mut child = test_task(
                "unrunnable-child",
                "unrunnable-root",
                Some(root.task_id.clone()),
            );
            let mut binding = magi_core::TaskExecutorBinding::for_role("executor");
            binding.agent_activation = Some(magi_core::AgentActivation {
                agent_id: child.task_id.clone(),
                session_id: magi_core::SessionId::new("unrunnable-session"),
                lifetime: magi_core::AgentLifetime::Session,
                generation: 1,
                previous_task_id: None,
            });
            child.executor_binding = Some(binding);
            store.insert_task(child.clone()).unwrap();
            let admission = Arc::new(ExecutionAdmissionController::default());
            admission.reserve(child.task_id.clone(), None, "executor");
            let runner =
                TaskRunner::with_dispatcher(store.clone(), vec![], Arc::new(RejectingDispatcher))
                    .with_execution_admission(admission.clone(), None);
            let RunCycleOutcome::Unrunnable(candidates) = runner.run_cycle(&root.task_id) else {
                panic!("missing worker must produce a candidate before the ownership change");
            };
            assert_eq!(candidates, [child.task_id.clone()]);
            match changed {
                "transferred" => {
                    store
                        .transfer_agent_subtree(
                            &root.task_id,
                            &child.task_id,
                            magi_core::AgentRunTransfer {
                                operation_id: "unrunnable-transfer".into(),
                                session_id: magi_core::SessionId::new("unrunnable-session"),
                                source_turn_id: "source-turn".into(),
                                transferred_by: root.task_id.clone(),
                                activation_generation: 1,
                                owner: magi_core::AgentRunOwner::Session,
                                transferred_at: UtcMillis::now(),
                            },
                        )
                        .unwrap();
                }
                "completed" => {
                    let lease = store
                        .claim_task_for_execution_scope(
                            &child.task_id,
                            &root.task_id,
                            &WorkerId::new("completion-worker"),
                            "executor",
                            60_000,
                        )
                        .unwrap()
                        .unwrap();
                    store
                        .complete_lease_and_task(
                            &child.task_id,
                            &root.task_id,
                            &lease.lease_id,
                            TaskCompletionAttempt {
                                output_refs: vec![],
                                final_response: Some("completed by another cycle".into()),
                                evidence: vec![],
                            },
                        )
                        .unwrap();
                }
                "started" => {
                    assert!(
                        store
                            .claim_task_for_execution_scope(
                                &child.task_id,
                                &root.task_id,
                                &WorkerId::new("late-worker"),
                                "executor",
                                60_000,
                            )
                            .unwrap()
                            .is_some()
                    );
                }
                _ => {}
            }
            assert_eq!(
                runner
                    .finalize_unrunnable_outcome(&root.task_id, &candidates)
                    .unwrap(),
                changed == "active"
            );
            assert_eq!(
                store.get_task(&root.task_id).unwrap().status,
                if changed == "active" {
                    TaskStatus::Failed
                } else {
                    TaskStatus::Running
                }
            );
            if changed == "transferred" {
                assert_eq!(
                    store.get_task(&child.task_id).unwrap().status,
                    TaskStatus::Pending
                );
                assert_eq!(admission.snapshot().active_task_count, 1);
            } else if changed == "active" {
                assert_eq!(admission.snapshot().active_task_count, 0);
            } else if changed == "started" {
                assert_eq!(
                    store.get_task(&child.task_id).unwrap().status,
                    TaskStatus::Running
                );
                assert_eq!(admission.snapshot().active_task_count, 1);
            }
        }
    }

    #[test]
    fn resumed_agent_parent_starts_before_its_pending_descendants() {
        let store = Arc::new(TaskStore::new());
        let root = test_task("resume-parent", "resume-parent", None);
        let child = test_task("resume-child", "resume-parent", Some(root.task_id.clone()));
        store.insert_task(root.clone()).unwrap();
        store.insert_task(child.clone()).unwrap();
        let runnable = store.get_runnable_leaves(&root.task_id);
        assert_eq!(
            runnable
                .iter()
                .map(|task| &task.task_id)
                .collect::<Vec<_>>(),
            vec![&root.task_id]
        );
        store
            .grant_lease_and_start_task(
                &root.task_id,
                &root.task_id,
                &WorkerId::new("resume-worker"),
                "executor",
                60_000,
            )
            .unwrap()
            .unwrap();
        let runnable = store.get_runnable_leaves(&root.task_id);
        assert_eq!(
            runnable
                .iter()
                .map(|task| &task.task_id)
                .collect::<Vec<_>>(),
            vec![&child.task_id]
        );
    }

    #[test]
    fn child_of_completed_parent_is_not_runnable() {
        let store = TaskStore::new();
        let mut root = test_task("task-root-done", "task-root-done", None);
        root.status = TaskStatus::Completed;
        let child = test_task(
            "task-late-child",
            "task-root-done",
            Some(root.task_id.clone()),
        );
        store.insert_task(root.clone()).expect("根任务应插入");
        store.insert_task(child).expect("子任务应插入");

        assert!(store.get_runnable_leaves(&root.task_id).is_empty());
    }

    #[test]
    fn dispatcher_receives_updates_committed_after_runnable_scan_before_lease() {
        struct SnapshotDispatcher;
        impl TaskDispatcher for SnapshotDispatcher {
            fn dispatch(
                &self,
                task: &Task,
                _worker: &WorkerInfo,
                _lease: &magi_orchestrator::task_store::TaskLease,
                _permit: ExecutionAdmissionPermit,
            ) -> Result<(), String> {
                assert_eq!(task.goal, "updated before grant");
                assert_eq!(task.status, TaskStatus::Running);
                Ok(())
            }
        }
        let store = Arc::new(TaskStore::new());
        let root = test_task("fresh-dispatch-root", "fresh-dispatch-root", None);
        store.insert_task(root.clone()).unwrap();
        let gate_store = Arc::clone(&store);
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            vec![WorkerInfo {
                worker_id: WorkerId::new("fresh-dispatch-worker"),
                role: "executor".into(),
                supported_kinds: vec![TaskKind::LocalAgent],
                parallelism_limit: None,
                system_prompt_template: None,
            }],
            Arc::new(SnapshotDispatcher),
        )
        .with_dispatch_gate(Arc::new(move |task| {
            gate_store
                .update_task_goal(&task.task_id, "updated before grant".into())
                .unwrap();
            Ok(TaskDispatchGateDecision::Allow)
        }));
        assert_eq!(runner.run_cycle(&root.task_id), RunCycleOutcome::Continue);
    }

    #[test]
    fn parent_closing_during_admission_does_not_fail_unrelated_branch() {
        struct RecordingDispatcher(Arc<Mutex<Vec<TaskId>>>);
        impl TaskDispatcher for RecordingDispatcher {
            fn dispatch(
                &self,
                task: &Task,
                _worker: &WorkerInfo,
                _lease: &magi_orchestrator::task_store::TaskLease,
                _permit: ExecutionAdmissionPermit,
            ) -> Result<(), String> {
                self.0.lock().unwrap().push(task.task_id.clone());
                Ok(())
            }
        }
        let store = Arc::new(TaskStore::new());
        let mut root = test_task("admission-root", "admission-root", None);
        root.status = TaskStatus::Running;
        let mut parent = test_task(
            "admission-parent",
            "admission-root",
            Some(root.task_id.clone()),
        );
        parent.status = TaskStatus::Running;
        let child = test_task(
            "admission-child",
            "admission-root",
            Some(parent.task_id.clone()),
        );
        let sibling = test_task(
            "admission-sibling",
            "admission-root",
            Some(root.task_id.clone()),
        );
        for task in [&root, &parent, &child, &sibling] {
            store.insert_task(task.clone()).unwrap();
        }
        let gate_store = Arc::clone(&store);
        let gate_child = child.task_id.clone();
        let gate_parent = parent.task_id.clone();
        let dispatched = Arc::new(Mutex::new(Vec::new()));
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            vec![WorkerInfo {
                worker_id: WorkerId::new("admission-worker"),
                role: "executor".into(),
                supported_kinds: vec![TaskKind::LocalAgent],
                parallelism_limit: None,
                system_prompt_template: None,
            }],
            Arc::new(RecordingDispatcher(Arc::clone(&dispatched))),
        )
        .with_dispatch_gate(Arc::new(move |task| {
            if task.task_id == gate_child {
                gate_store
                    .complete_task(
                        &gate_parent,
                        TaskCompletionAttempt {
                            final_response: Some("父任务已在接纳窗口完成".into()),
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            Ok(TaskDispatchGateDecision::Allow)
        }));
        assert_eq!(runner.run_cycle(&root.task_id), RunCycleOutcome::Continue);
        assert_eq!(*dispatched.lock().unwrap(), vec![sibling.task_id]);
        assert!(store.get_active_lease(&child.task_id).is_none());
        assert_eq!(
            store.get_task(&child.task_id).unwrap().status,
            TaskStatus::Pending
        );
        assert_eq!(
            store.get_task(&root.task_id).unwrap().status,
            TaskStatus::Running
        );
    }

    #[test]
    fn dispatch_gate_blocks_without_lease_or_status_change() {
        let store = Arc::new(TaskStore::new());
        let root = test_task("task-root-gated", "task-root-gated", None);
        store.insert_task(root.clone()).expect("根任务应插入");
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(RejectingDispatcher),
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
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(RejectingDispatcher),
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
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            vec![executor],
            Arc::new(RejectingDispatcher),
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
        let runner = TaskRunner::with_dispatcher(Arc::clone(&store), Vec::new(), dispatcher)
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
    fn cancellation_receipt_is_lease_bound_idempotent_and_cannot_become_failure() {
        let store = TaskStore::new();
        let mut task = test_task("cancel-receipt", "cancel-receipt", None);
        task.status = TaskStatus::Running;
        store.insert_task(task.clone()).unwrap();
        let worker = WorkerId::new("cancel-worker");
        let old = store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker,
                "executor",
                DEFAULT_LEASE_DURATION_MS,
            )
            .unwrap();
        assert!(store.revoke_lease(&task.task_id, &old.lease_id));
        let current = store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker,
                "executor",
                DEFAULT_LEASE_DURATION_MS,
            )
            .unwrap();
        assert!(
            !apply_task_result(
                &store,
                TaskResult {
                    task_id: task.task_id.clone(),
                    lease_id: old.lease_id,
                    outcome: TaskOutcome::Cancelled,
                }
            )
            .unwrap()
        );
        assert_eq!(
            store.get_task(&task.task_id).unwrap().status,
            TaskStatus::Running
        );
        assert!(
            apply_task_result(
                &store,
                TaskResult {
                    task_id: task.task_id.clone(),
                    lease_id: current.lease_id.clone(),
                    outcome: TaskOutcome::Cancelled,
                }
            )
            .unwrap()
        );
        for outcome in [
            TaskOutcome::Cancelled,
            TaskOutcome::Failed {
                error: "late failure".into(),
            },
        ] {
            assert!(
                !apply_task_result(
                    &store,
                    TaskResult {
                        task_id: task.task_id.clone(),
                        lease_id: current.lease_id.clone(),
                        outcome,
                    }
                )
                .unwrap()
            );
        }
        assert_eq!(
            store.get_task(&task.task_id).unwrap().status,
            TaskStatus::Killed
        );
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
        let result = apply_task_result(
            &store,
            TaskResult {
                task_id: root.task_id.clone(),
                lease_id: stale_lease.lease_id,
                outcome: TaskOutcome::Failed {
                    error: "旧执行轮迟到失败".to_string(),
                },
            },
        );
        assert!(!result.unwrap(), "迟到结果不能修改当前任务或租约");
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(RejectingDispatcher),
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

        let result = apply_task_result(
            &store,
            TaskResult {
                task_id: root.task_id.clone(),
                lease_id: lease.lease_id.clone(),
                outcome: TaskOutcome::Completed {
                    attempt: TaskCompletionAttempt {
                        output_refs: vec!["output://completion".to_string()],
                        final_response: Some("完成".to_string()),
                        evidence: Vec::new(),
                    },
                },
            },
        );
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(RejectingDispatcher),
        );

        let error = result.unwrap_err();
        assert!(error.contains("完成事实持久化失败"), "{error}");
        assert!(
            error.contains("completion checkpoint unavailable"),
            "{error}"
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
                .expect("lease should remain active")
                .lease_id,
            lease.lease_id
        );
    }

    #[test]
    fn concurrent_runners_claim_one_task_once_without_a_conversation_lock() {
        let store = Arc::new(TaskStore::new());
        let task = test_task("single-dispatch", "single-dispatch", None);
        store.insert_task(task.clone()).unwrap();
        let dispatcher = Arc::new(HoldingDispatcher {
            permits: Mutex::new(Vec::new()),
        });
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let outcomes = std::thread::scope(|scope| {
            (0..8)
                .map(|_| {
                    let store = store.clone();
                    let dispatcher = dispatcher.clone();
                    let barrier = barrier.clone();
                    let task_id = task.task_id.clone();
                    scope.spawn(move || {
                        TaskRunner::with_dispatcher(
                            store,
                            vec![WorkerInfo {
                                worker_id: WorkerId::new("single-dispatch-worker"),
                                role: "executor".into(),
                                supported_kinds: vec![TaskKind::LocalAgent],
                                parallelism_limit: None,
                                system_prompt_template: None,
                            }],
                            dispatcher,
                        )
                        // 各 Runner 使用独立准入器，强制验证 TaskStore 的原子租约，
                        // 不能由共享容量计数碰巧挡住重复派发。
                        .with_execution_admission(
                            Arc::new(ExecutionAdmissionController::new(
                                ExecutionAdmissionLimits {
                                    min_available_memory_bytes: 0,
                                    ..Default::default()
                                },
                            )),
                            None,
                        )
                        .with_dispatch_gate(Arc::new(move |_| {
                            barrier.wait();
                            Ok(TaskDispatchGateDecision::Allow)
                        }))
                        .run_cycle(&task_id)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|run| run.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == RunCycleOutcome::Continue)
                .count(),
            1,
            "{outcomes:?}"
        );
        assert_eq!(dispatcher.permits.lock().unwrap().len(), 1);
        assert_eq!(
            store.get_task(&task.task_id).unwrap().status,
            TaskStatus::Running
        );
        assert!(store.get_active_lease(&task.task_id).is_some());
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
        let first_runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            vec![worker.clone()],
            dispatcher.clone(),
        )
        .with_execution_admission(
            Arc::clone(&controller),
            Some(SessionId::new("session-admission-first")),
        );
        let second_runner =
            TaskRunner::with_dispatcher(Arc::clone(&store), vec![worker], dispatcher.clone())
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
