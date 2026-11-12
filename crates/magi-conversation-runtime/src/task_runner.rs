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
            RunCycleOutcome::Waiting
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
mod tests;
