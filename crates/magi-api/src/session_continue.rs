//! 继续会话 API 适配层。
//!
//! 纯判定、校验、writeback 落盘、子树解封逻辑已下沉到
//! `magi_conversation_runtime::execution_chain_recovery`；本模块只负责把 `ApiState`
//! 持有的 runner、task store 与 execution registry 装配给 runtime 恢复流程。

use crate::{
    errors::ApiError,
    state::{ApiState, RunnerStartError},
};
use magi_conversation_runtime::{
    CoordinatorCommandResult, CoordinatorError, CoordinatorTurnStatus, ExecutionProfile,
    SessionTurnCoordinator, TurnAdmission, TurnAttempt, TurnCommand,
    execution_chain_recovery::{
        apply_chain_recovery_if_needed, commit_chain_recovery, fail_resumed_execution_paths,
        release_resumed_branch_path,
    },
    task_execution_registry::TaskExecutionPlan,
};
use magi_core::{
    SessionId, SessionLifecycleStatus, TaskExecutionTarget, TaskStatus, UtcMillis, WorkerId,
};
use magi_orchestrator::ExecutionWritebackPlans;
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ExecutionThread, ExecutionThreadStatus,
    InterruptedGoalResumeCheckpoint, SessionStore,
};
use magi_settings_store::SettingsStore;
use std::sync::Arc;

struct InterruptedRecoveryClaimGuard {
    session_store: SessionStore,
    session_id: SessionId,
    turn_id: Option<String>,
    committed: bool,
}

impl InterruptedRecoveryClaimGuard {
    fn claim(session_store: &SessionStore, session_id: &SessionId) -> Result<Self, ApiError> {
        let turn_id = session_store
            .claim_interrupted_recovery(session_id)
            .map_err(|error| ApiError::conflict("继续会话失败", &error.to_string()))?;
        Ok(Self {
            session_store: session_store.clone(),
            session_id: session_id.clone(),
            turn_id,
            committed: false,
        })
    }

    fn commit(mut self) {
        self.committed = true;
    }

    fn turn_id(&self) -> Option<&str> {
        self.turn_id.as_deref()
    }
}

impl Drop for InterruptedRecoveryClaimGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let Some(turn_id) = self.turn_id.as_deref() else {
            return;
        };
        if let Err(error) = self
            .session_store
            .release_interrupted_recovery_claim(&self.session_id, turn_id)
        {
            tracing::error!(
                ?error,
                session_id = %self.session_id,
                turn_id,
                "释放异常中断恢复领取权失败"
            );
        }
    }
}

struct InterruptedGoalResumeGuard {
    session_store: SessionStore,
    checkpoint: Option<InterruptedGoalResumeCheckpoint>,
    committed: bool,
}

impl InterruptedGoalResumeGuard {
    fn prepare(
        session_store: &SessionStore,
        session_id: &SessionId,
        interrupted_turn_id: Option<&str>,
        resumed_turn_id: &str,
        resumed_at: UtcMillis,
    ) -> Result<Self, ApiError> {
        let checkpoint = interrupted_turn_id
            .map(|interrupted_turn_id| {
                session_store.resume_goal_for_interrupted_execution(
                    session_id,
                    interrupted_turn_id,
                    resumed_turn_id,
                    resumed_at,
                )
            })
            .transpose()
            .map_err(|error| ApiError::conflict("继续会话失败", &error.to_string()))?
            .flatten();
        Ok(Self {
            session_store: session_store.clone(),
            checkpoint,
            committed: false,
        })
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for InterruptedGoalResumeGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let Some(checkpoint) = self.checkpoint.take() else {
            return;
        };
        if let Err(error) = self
            .session_store
            .rollback_interrupted_goal_resume(checkpoint)
        {
            tracing::error!(?error, "回滚异常中断关联 Goal 失败");
        }
    }
}

/// Continue 的恢复尝试边界：在 runner 成功启动并完成 recovery 提交前，任何错误都必须
/// 清理本轮执行计划、把已重新打开的任务路径收口为 Failed，并把新 Turn 收口为 failed。
/// Drop 语义保证所有 `?` 路径都走同一失败收口，不依赖调用方记住某个补丁分支。
struct ContinueRecoveryAttempt<'a> {
    state: &'a ApiState,
    session_id: SessionId,
    chain: ActiveExecutionChain,
    branches: Vec<ActiveExecutionBranch>,
    resumed_turn_id: String,
    coordinator: Option<&'a SessionTurnCoordinator>,
    coordinator_attempt: Option<TurnAttempt>,
    registered_task_ids: Vec<magi_core::TaskId>,
    registered_threads: Vec<ExecutionThread>,
    committed: bool,
}

impl<'a> ContinueRecoveryAttempt<'a> {
    fn new(
        state: &'a ApiState,
        session_id: &SessionId,
        chain: &ActiveExecutionChain,
        branches: &[ActiveExecutionBranch],
        resumed_turn_id: &str,
        coordinator: Option<&'a SessionTurnCoordinator>,
    ) -> Self {
        Self {
            state,
            session_id: session_id.clone(),
            chain: chain.clone(),
            branches: branches.to_vec(),
            resumed_turn_id: resumed_turn_id.to_string(),
            coordinator,
            coordinator_attempt: None,
            registered_task_ids: Vec::new(),
            registered_threads: Vec::new(),
            committed: false,
        }
    }

    fn record_coordinator_attempt(&mut self, attempt: TurnAttempt) {
        self.coordinator_attempt = Some(attempt);
    }

    fn record_registered_task(&mut self, task_id: magi_core::TaskId) {
        self.registered_task_ids.push(task_id);
    }

    fn record_registered_thread(&mut self, thread: ExecutionThread) {
        self.registered_threads.push(thread);
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for ContinueRecoveryAttempt<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Some(task_store) = self.state.task_store()
            && let Err(error) =
                fail_resumed_execution_paths(task_store, &self.chain, &self.branches)
        {
            tracing::error!(
                ?error,
                session_id = %self.session_id,
                root_task_id = %self.chain.root_task_id,
                "Continue 恢复失败后的任务路径收口失败"
            );
        }
        for thread in &self.registered_threads {
            let Some(branch) = self
                .branches
                .iter()
                .find(|branch| branch.thread_id == thread.thread_id)
            else {
                tracing::error!(
                    thread_id = %thread.thread_id,
                    session_id = %self.session_id,
                    "Continue 恢复失败后的 thread 回滚缺少 branch 所有权信息"
                );
                continue;
            };
            if let Err(error) = self.state.session_store.remove_recovery_thread_if_owned(
                &self.session_id,
                &thread.thread_id,
                &thread.mission_id,
                &branch.task_id,
                &thread.worker_instance_id,
            ) {
                tracing::error!(
                    ?error,
                    thread_id = %thread.thread_id,
                    task_id = %branch.task_id,
                    session_id = %self.session_id,
                    turn_id = %self.resumed_turn_id,
                    "Continue 恢复失败后的 thread 回滚所有权校验失败"
                );
            }
        }
        for task_id in &self.registered_task_ids {
            if self
                .state
                .task_execution_registry()
                .remove_if_turn_matches(task_id, &self.resumed_turn_id)
                .is_none()
            {
                tracing::debug!(
                    %task_id,
                    session_id = %self.session_id,
                    turn_id = %self.resumed_turn_id,
                    "Continue 恢复失败后的执行计划已经被清理"
                );
            }
        }
        fail_prepared_continue_turn(self.state, &self.session_id, &self.resumed_turn_id);
        if let (Some(coordinator), Some(attempt)) =
            (self.coordinator, self.coordinator_attempt.as_ref())
        {
            let terminal = self
                .state
                .session_store
                .canonical_turn_for_session_turn_id(&self.session_id, &self.resumed_turn_id)
                .and_then(|turn| match turn.status {
                    magi_session_store::CanonicalTurnStatus::Completed => {
                        Some(CoordinatorTurnStatus::Completed)
                    }
                    magi_session_store::CanonicalTurnStatus::Failed => {
                        Some(CoordinatorTurnStatus::Failed)
                    }
                    magi_session_store::CanonicalTurnStatus::Interrupted
                    | magi_session_store::CanonicalTurnStatus::Cancelled
                    | magi_session_store::CanonicalTurnStatus::Superseded => {
                        Some(CoordinatorTurnStatus::Cancelled)
                    }
                    _ => None,
                });
            let result = match terminal {
                Some(status) => coordinator
                    .execute_command(
                        &self.session_id,
                        TurnCommand::Finish {
                            attempt: attempt.clone(),
                            status,
                        },
                    )
                    .map(|_| ()),
                None => coordinator
                    .execute_command(
                        &self.session_id,
                        TurnCommand::Abort {
                            attempt: attempt.clone(),
                        },
                    )
                    .map(|_| ()),
            };
            if let Err(error) = result {
                tracing::error!(
                    ?error,
                    session_id = %self.session_id,
                    turn_id = %self.resumed_turn_id,
                    "Continue 恢复失败后的 Coordinator 收口失败"
                );
            }
        }
        if let Err(error) = self
            .state
            .persist_runtime_durable_state_for_sessions_for_api(std::slice::from_ref(
                &self.session_id,
            ))
        {
            tracing::error!(
                ?error,
                session_id = %self.session_id,
                "Continue 恢复失败后的 durable 状态持久化失败"
            );
        }
    }
}

// 对 routes/sessions.rs 暴露继续会话所需的 runtime 数据载体与判定函数。
pub(crate) use magi_conversation_runtime::execution_chain_recovery::{
    SessionContinueAccepted, active_execution_branch_is_continue_recoverable,
    task_status_is_terminal,
};

/// 新 Turn 已经写入后，如果 runner 启动失败，必须把它收口为失败态。
///
/// Continue 入口会先持久化用户可见的 running Turn，再启动后台 runner；启动阶段的
/// 任何失败都不能把这个 Turn 留在 running，否则下一次发送会继续看到一个永远不会
/// 收口的“活动会话”，并再次触发旧执行链的恢复逻辑。
fn fail_prepared_continue_turn(state: &ApiState, session_id: &SessionId, turn_id: &str) {
    match state
        .turn_event_sink()
        .set_status_domain(session_id, Some(turn_id), "failed")
    {
        Ok(Some(_)) => {
            if let Err(error) =
                state.persist_session_state_checkpoint("session_continue_start_failed")
            {
                tracing::error!(
                    ?error,
                    %session_id,
                    turn_id,
                    "Continue runner 启动失败后的 Turn 终态持久化失败"
                );
            }
        }
        Ok(None) => {
            tracing::warn!(
                %session_id,
                turn_id,
                "Continue runner 启动失败时未找到可收口的 current Turn"
            );
        }
        Err(error) => {
            tracing::error!(
                ?error,
                %session_id,
                turn_id,
                "Continue runner 启动失败后的 Turn 收口失败"
            );
        }
    }
}

fn rebuild_dispatch_plan_for_branch(
    chain: &ActiveExecutionChain,
    branch: &ActiveExecutionBranch,
    recovery_id: Option<String>,
    turn_id: &str,
    execution_root: Option<std::path::PathBuf>,
    execution_settings_snapshot: Option<Arc<SettingsStore>>,
) -> TaskExecutionPlan {
    let writebacks = if branch.is_primary {
        ExecutionWritebackPlans::from_session_action_input(
            magi_orchestrator::DispatchMemoryExtractionInput {
                accepted_at: chain.dispatch_context.accepted_at,
                session_id: &chain.session_id,
                timeline_entry_id: chain.dispatch_context.entry_id.as_str(),
                text: chain.dispatch_context.trimmed_text.as_deref(),
                skill_name: chain.dispatch_context.skill_name.as_deref(),
            },
        )
    } else {
        ExecutionWritebackPlans::default()
    };
    // 恢复链路的 thread_id：直接读 branch.thread_id。`ensure_thread_for_role`
    // 用 `now.0` 拼 id 不可重放，必须持久化在 branch。
    TaskExecutionPlan::Dispatch {
        target: TaskExecutionTarget {
            mission_id: chain.mission_id.clone(),
            root_task_id: chain.root_task_id.clone(),
            task_id: branch.task_id.clone(),
            requested_worker_id: Some(branch.worker_id.clone()),
            recovery_id,
            execution_chain_ref: Some(chain.execution_chain_ref.clone()),
        },
        worker_id: branch.worker_id.clone(),
        thread_id: branch.thread_id.clone(),
        is_primary: branch.is_primary,
        session_id: chain.session_id.clone(),
        turn_id: turn_id.to_string(),
        workspace_id: chain.workspace_id.clone(),
        execution_root,
        writebacks,
        use_tools: branch.use_tools,
        skill_name: branch.skill_name.clone(),
        images: Vec::new(),
        execution_settings_snapshot,
    }
}

pub(crate) fn restore_resumed_branch_threads(
    state: &ApiState,
    session_id: &SessionId,
    chain: &ActiveExecutionChain,
    branches: &[ActiveExecutionBranch],
) -> Result<Vec<ExecutionThread>, ApiError> {
    if &chain.session_id != session_id {
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            "恢复执行链不属于当前会话",
        ));
    }
    let registered_threads = state.session_store.thread_registry_snapshot(session_id);
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("继续会话失败", "task_store 未配置"))?;
    let canonical_turns = state.session_store.canonical_turns_for_session(session_id);
    let mut threads_to_register = Vec::new();
    let mut branch_threads = std::collections::HashSet::new();
    for branch in branches {
        if !branch_threads.insert(&branch.thread_id) {
            return Err(ApiError::internal_assembly(
                "继续会话失败",
                format!("恢复分支重复使用 thread: {}", branch.thread_id),
            ));
        }
        let task = task_store.get_task(&branch.task_id).ok_or_else(|| {
            ApiError::not_found("恢复 branch 任务不存在", branch.task_id.as_str())
        })?;
        if task.mission_id != chain.mission_id || task.root_task_id != chain.root_task_id {
            return Err(ApiError::internal_assembly(
                "继续会话失败",
                format!("恢复 branch 不属于当前执行链: {}", branch.task_id),
            ));
        }
        if let Some(thread) = registered_threads
            .iter()
            .find(|thread| thread.thread_id == branch.thread_id)
        {
            // 主线 thread 归 Session 所有，常驻 worker 身份不等于本次派发 worker；
            // 独立任务 thread 才由 branch worker 独占。
            let owner_matches = if thread.role_id == magi_session_store::ORCHESTRATOR_ROLE_ID {
                branch.is_primary && task.executor_binding_target_role() == Some("coordinator")
            } else {
                thread.worker_instance_id == branch.worker_id
            };
            if thread.mission_id != chain.mission_id
                || !owner_matches
                || !thread.handled_task_ids.contains(&branch.task_id)
            {
                return Err(ApiError::internal_assembly(
                    "继续会话失败",
                    format!("恢复分支 thread 归属不匹配: {}", branch.thread_id),
                ));
            }
            continue;
        }
        let created_at = canonical_turns
            .iter()
            .flat_map(|turn| turn.items.iter())
            .filter(|item| item.source_thread_id == branch.thread_id)
            .map(|item| item.created_at)
            .min_by_key(|time| time.0)
            .unwrap_or(chain.dispatch_context.accepted_at);
        let thread = ExecutionThread {
            thread_id: branch.thread_id.clone(),
            session_id: session_id.clone(),
            mission_id: chain.mission_id.clone(),
            role_id: task
                .executor_binding_target_role()
                .unwrap_or("coordinator")
                .to_string(),
            worker_instance_id: branch.worker_id.clone(),
            status: ExecutionThreadStatus::Active,
            created_at,
            last_used_at: UtcMillis::now(),
            observed_context_window_tokens: None,
            handled_task_ids: vec![branch.task_id.clone()],
        };
        state
            .session_store
            .canonical_thread_message_history(&thread)
            .map_err(|error| ApiError::internal_assembly("重建恢复线程历史失败", error))?;
        threads_to_register.push(thread);
    }

    let mut registered_threads = Vec::with_capacity(threads_to_register.len());
    for thread in threads_to_register {
        if let Err(error) = state.session_store.register_thread(thread.clone()) {
            let rollback_errors =
                rollback_restored_branch_threads(state, session_id, chain, &registered_threads);
            let detail = if rollback_errors.is_empty() {
                String::new()
            } else {
                format!("；回滚失败: {}", rollback_errors.join("；"))
            };
            return Err(ApiError::internal_assembly(
                "继续会话失败",
                format!("恢复分支注册 thread 失败: {error}{detail}"),
            ));
        }
        registered_threads.push(thread);
    }
    if !registered_threads.is_empty()
        && let Err(error) =
            state.persist_session_state_checkpoint("session_continue_thread_rebuild")
    {
        let rollback_errors =
            rollback_restored_branch_threads(state, session_id, chain, &registered_threads);
        let detail = if rollback_errors.is_empty() {
            String::new()
        } else {
            format!("；回滚失败: {}", rollback_errors.join("；"))
        };
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            format!("恢复分支 thread 状态持久化失败: {error:?}{detail}"),
        ));
    }
    Ok(registered_threads)
}

fn rollback_restored_branch_threads(
    state: &ApiState,
    session_id: &SessionId,
    chain: &ActiveExecutionChain,
    threads: &[ExecutionThread],
) -> Vec<String> {
    threads
        .iter()
        .filter_map(|thread| {
            let branch = chain
                .branches
                .iter()
                .find(|branch| branch.thread_id == thread.thread_id)?;
            match state.session_store.remove_recovery_thread_if_owned(
                session_id,
                &thread.thread_id,
                &thread.mission_id,
                &branch.task_id,
                &thread.worker_instance_id,
            ) {
                Ok(Some(_)) | Ok(None) => None,
                Err(error) => Some(format!("thread {}: {error}", thread.thread_id)),
            }
        })
        .collect()
}

/// 在持有 session 生命周期锁且领取恢复权后、启动新 runner 前写入本轮用户信号。
///
/// 这使“用户输入接管中断任务”和“启动恢复 runner”属于同一个串行临界区，避免双击
/// 恢复链接或多个窗口同时提交时把第二条输入遗留到错误执行链中。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn continue_execution_chain_with_pre_resume<U, G>(
    state: &ApiState,
    session_id: &SessionId,
    requested_agent_ids: &[WorkerId],
    resumed_turn_id: &str,
    resumed_at: UtcMillis,
    request_id: String,
    request_fingerprint: String,
    prepare_turn: G,
) -> Result<(SessionContinueAccepted, U), ApiError>
where
    G: FnOnce(
        &ActiveExecutionChain,
        &ActiveExecutionBranch,
        &[ActiveExecutionBranch],
        &TurnAttempt,
    ) -> Result<U, ApiError>,
{
    if state.session_store.session(session_id).is_none() {
        return Err(ApiError::session_not_found(session_id.as_str()));
    }
    let sidecar = state
        .session_store
        .runtime_sidecar(session_id)
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可继续的执行链".to_string()))?;
    let mut chain = sidecar
        .active_execution_chain
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可继续的执行链".to_string()))?;
    if &chain.session_id != session_id {
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            format!(
                "session sidecar 与 active execution chain 不一致: {} != {}",
                chain.session_id, session_id
            ),
        ));
    }
    if let Some(ownership_chain_ref) = sidecar.ownership.execution_chain_ref.as_deref()
        && ownership_chain_ref != chain.execution_chain_ref
    {
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            format!(
                "session sidecar 的 execution_chain_ref 与 active chain 不一致: {} != {}",
                ownership_chain_ref, chain.execution_chain_ref
            ),
        ));
    }

    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("继续会话失败", "task_store 未配置"))?;
    let root_task = task_store
        .get_task(&chain.root_task_id)
        .ok_or_else(|| ApiError::not_found("根任务不存在", chain.root_task_id.as_str()))?;
    if root_task.mission_id != chain.mission_id {
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            format!(
                "active chain 的 mission_id 与根任务不一致: {} != {}",
                chain.mission_id, root_task.mission_id
            ),
        ));
    }
    let manager = state
        .runner_manager()
        .ok_or_else(|| ApiError::internal_assembly("继续会话失败", "runner_manager 未配置"))?;
    let _runner_lifecycle_guard = state
        .lock_runner_lifecycle_after_turn_commit(session_id)
        .await
        .ok_or_else(|| ApiError::internal_assembly("继续会话失败", "runner_manager 未配置"))?;
    let session = state
        .session_store
        .session(session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    if session.status != SessionLifecycleStatus::Active {
        return Err(ApiError::InvalidInput(
            "当前会话已关闭，不能继续执行".to_string(),
        ));
    }
    // Session lifecycle lock 必须先于 root restart lock；在拿到两把锁之前只允许读取状态。
    chain = state
        .session_store
        .active_execution_chain(session_id)
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可继续的执行链".to_string()))?;
    let root_task = task_store
        .get_task(&chain.root_task_id)
        .ok_or_else(|| ApiError::not_found("根任务不存在", chain.root_task_id.as_str()))?;
    if root_task.mission_id != chain.mission_id {
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            format!(
                "active chain 的 mission_id 与根任务不一致: {} != {}",
                chain.mission_id, root_task.mission_id
            ),
        ));
    }
    let _restart_guard = manager.lock_for_restart(chain.root_task_id.as_str()).await;
    // 领取时会校验原 Turn 的待审批/提问；必须早于任务恢复、检查点
    // 改写和 Coordinator Continue 接纳。拒绝不能留下一个没有 canonical
    // Turn 的新 Coordinator attempt，阻塞用户处理交互后的合法恢复。
    let recovery_claim = InterruptedRecoveryClaimGuard::claim(&state.session_store, session_id)?;
    manager
        .quiesce_for_restart(chain.root_task_id.as_str())
        .await;

    let recovery_sidecar = state
        .session_store
        .runtime_sidecar(session_id)
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可继续的执行链".to_string()))?;
    chain = recovery_sidecar
        .active_execution_chain
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可继续的执行链".to_string()))?;
    let root_task = task_store
        .get_task(&chain.root_task_id)
        .ok_or_else(|| ApiError::not_found("根任务不存在", chain.root_task_id.as_str()))?;
    if root_task.mission_id != chain.mission_id {
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            format!(
                "active chain 的 mission_id 与根任务不一致: {} != {}",
                chain.mission_id, root_task.mission_id
            ),
        ));
    }
    let resumable_branches = chain
        .branches
        .iter()
        .filter(|&branch| {
            active_execution_branch_is_continue_recoverable(
                state.task_store(),
                &chain,
                recovery_sidecar.current_turn.as_ref(),
                branch,
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let branches_to_resume = if requested_agent_ids.is_empty() {
        resumable_branches
    } else {
        resumable_branches
            .into_iter()
            .filter(|branch| {
                requested_agent_ids
                    .iter()
                    .any(|agent_id| agent_id == &branch.worker_id)
            })
            .collect::<Vec<_>>()
    };
    if branches_to_resume.is_empty() {
        return Err(ApiError::InvalidInput(
            "执行状态已经变化，当前没有可继续的 branch".to_string(),
        ));
    }

    let workspace_id = state
        .session_workspace_id(&session)
        .or_else(|| chain.workspace_id.clone());
    let binding = crate::task_dispatch::validate_or_migrate_directory_binding(
        state,
        workspace_id.as_ref(),
        root_task
            .executor_binding
            .as_ref()
            .and_then(|binding| binding.project_directories.clone()),
    )?;
    let execution_root = if let Some(binding) = binding.as_ref() {
        Some(crate::task_dispatch::bound_project_execution_root(
            state, session_id, binding,
        )?)
    } else {
        Some(state.personal_session_execution_root(session_id)?)
    };

    let primary_branch = branches_to_resume
        .iter()
        .find(|branch| {
            requested_agent_ids
                .iter()
                .any(|agent_id| agent_id == &branch.worker_id)
        })
        .or_else(|| branches_to_resume.iter().find(|branch| branch.is_primary))
        .or_else(|| branches_to_resume.first())
        .expect("branches_to_resume checked as non-empty");
    let memory_store = state
        .execution_pipeline()
        .map(|pipeline| &pipeline.memory_store);
    let recovery_commit = apply_chain_recovery_if_needed(
        &state.session_store,
        &state.workspace_registry,
        memory_store,
        session_id,
        &mut chain,
        primary_branch,
    )
    .map_err(|error| {
        let message = error.into_message();
        // 与原实现保持一致：NotFound 与 InvalidStatus 走 InvalidInput / NotFound 分类。
        if message.starts_with("recovery 不存在") {
            ApiError::recovery_not_found(
                message
                    .strip_prefix("recovery 不存在: ")
                    .unwrap_or(message.as_str()),
            )
        } else if message.contains("继续检查点")
            || message.contains("恢复入口")
            || message.contains("workspace 不一致")
        {
            ApiError::InvalidInput(message)
        } else {
            ApiError::internal_assembly("继续会话失败", message)
        }
    })?;

    // 恢复链可能已经因为任务失败或 daemon 停止而失去执行能力，但旧 current Turn
    // 的异步终态写回尚未到达。先按 execution chain 携带的 Turn ID 原子收口旧轮次，
    // 再写入 Continue 的新 Turn，避免新旧 Turn 竞争同一个 current_turn 指针。
    if let Some(previous_turn_id) = chain.current_turn_id.as_deref() {
        state
            .turn_event_sink()
            .finalize_turn_for_continue(session_id, previous_turn_id)
            .map_err(|error| ApiError::internal_assembly("收口 Continue 前置 Turn 失败", error))?;
        state.persist_session_state_checkpoint("session_continue_finalize_previous_turn")?;
    }

    // 旧 current Turn 可能已经由 SessionStore 收口，但 daemon 恢复或异步终态回调
    // 尚未释放 Coordinator active 槽位。先读取旧 attempt 和 canonical 终态，
    // 最后通过同一个 Continue command 原子收口并接纳新 Turn。
    let mut previous_coordinator = None;
    if let Some(previous_turn_id) = chain.current_turn_id.as_deref() {
        let previous_turn = state
            .session_store
            .canonical_turn_for_session_turn_id(session_id, previous_turn_id)
            .ok_or_else(|| {
                ApiError::internal_assembly("继续会话失败", "Continue 来源 canonical Turn 不存在")
            })?;
        let previous_status = match previous_turn.status {
            magi_session_store::CanonicalTurnStatus::Completed => CoordinatorTurnStatus::Completed,
            magi_session_store::CanonicalTurnStatus::Failed => CoordinatorTurnStatus::Failed,
            magi_session_store::CanonicalTurnStatus::Cancelled
            | magi_session_store::CanonicalTurnStatus::Interrupted
            | magi_session_store::CanonicalTurnStatus::Superseded => {
                CoordinatorTurnStatus::Cancelled
            }
            _ => {
                return Err(ApiError::internal_assembly(
                    "继续会话失败",
                    "Continue 来源 Turn 尚未结算",
                ));
            }
        };
        match state
            .turn_coordinator()
            .current_attempt(session_id, previous_turn_id)
        {
            Ok(previous_attempt) => {
                previous_coordinator = Some((previous_attempt, previous_status));
            }
            Err(CoordinatorError::NoActiveTurn) => {}
            Err(error) => {
                return Err(ApiError::conflict(
                    "Continue 旧 Turn 与 Coordinator 不一致",
                    &error.to_string(),
                ));
            }
        }
    }
    let next_admission = TurnAdmission {
        turn_id: resumed_turn_id.to_string(),
        request_id,
        request_fingerprint,
        profile: ExecutionProfile::Task,
    };
    let command = match previous_coordinator {
        Some((previous, previous_status)) => TurnCommand::Continue {
            previous,
            previous_status,
            next: next_admission,
        },
        None => TurnCommand::Start(next_admission),
    };
    let coordinator_admission = match state
        .turn_coordinator()
        .execute_command(session_id, command)
        .map_err(|error| ApiError::Conflict(error.to_string()))?
    {
        CoordinatorCommandResult::Admission(admission) => admission,
        other => {
            return Err(ApiError::internal_assembly(
                "接纳 Continue Turn",
                format!("Coordinator 返回了非法 Continue 接纳结果: {other:?}"),
            ));
        }
    };
    let coordinator_attempt = match coordinator_admission {
        magi_conversation_runtime::CoordinatorAdmission::Accepted(attempt) => attempt,
        magi_conversation_runtime::CoordinatorAdmission::Replay(attempt) => {
            return Err(ApiError::Conflict(format!(
                "Continue Turn {} 已经接纳（attempt {}），请重放原请求读取 canonical receipt",
                attempt.turn_id, attempt.attempt_id
            )));
        }
    };
    if let Some(notifier) = state.task_completion_notifier() {
        notifier.bind_coordinator_attempt(
            &chain.root_task_id,
            &coordinator_attempt.attempt_id,
            Some(session_id.clone()),
            Some(resumed_turn_id.to_string()),
        );
    }

    let mut recovery_attempt = ContinueRecoveryAttempt::new(
        state,
        session_id,
        &chain,
        &branches_to_resume,
        resumed_turn_id,
        Some(state.turn_coordinator()),
    );
    recovery_attempt.record_coordinator_attempt(coordinator_attempt.clone());
    for thread in restore_resumed_branch_threads(state, session_id, &chain, &branches_to_resume)? {
        recovery_attempt.record_registered_thread(thread);
    }
    let goal_resume_guard = InterruptedGoalResumeGuard::prepare(
        &state.session_store,
        session_id,
        recovery_claim.turn_id(),
        resumed_turn_id,
        resumed_at,
    )?;

    // resume 入口幂等地保证 orchestrator thread 存在：
    //  * 已存在 → 直接复用 (同 session 同 mission 同 orchestrator thread 不变量)；
    //  * 不存在 → 用 chain.mission_id spawn 新 thread。
    // thread 自身由 branch.thread_id 承载，本调用仅维护 mission orchestrator thread 存在性。
    state.session_store.ensure_session_mission(
        session_id,
        chain.dispatch_context.accepted_at,
        || chain.mission_id.clone(),
    );

    let mut root_status = root_task.status;
    if matches!(root_status, TaskStatus::Completed) {
        task_store
            .reopen_completed_root_for_recovery(&chain.root_task_id)
            .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;
        root_status = TaskStatus::Failed;
    } else if task_status_is_terminal(&root_status) {
        return Err(ApiError::InvalidInput(
            "当前会话执行链已结束，不能继续".to_string(),
        ));
    }

    let execution_settings_snapshot = Some(Arc::new(state.settings_store.execution_snapshot()));
    for branch in &branches_to_resume {
        if state
            .task_execution_registry()
            .insert(
                branch.task_id.clone(),
                rebuild_dispatch_plan_for_branch(
                    &chain,
                    branch,
                    recovery_sidecar.recovery_id.clone(),
                    resumed_turn_id,
                    execution_root.clone(),
                    execution_settings_snapshot.clone(),
                ),
            )
            .is_err()
        {
            return Err(ApiError::internal_assembly(
                "继续会话失败",
                format!("恢复分支 {} 已存在执行计划，拒绝重复注册", branch.task_id),
            ));
        }
        recovery_attempt.record_registered_task(branch.task_id.clone());
    }

    state
        .session_store
        .apply_resume_execution_target(
            session_id,
            &TaskExecutionTarget {
                mission_id: chain.mission_id.clone(),
                root_task_id: chain.root_task_id.clone(),
                task_id: primary_branch.task_id.clone(),
                requested_worker_id: Some(primary_branch.worker_id.clone()),
                recovery_id: recovery_sidecar.recovery_id.clone(),
                execution_chain_ref: Some(chain.execution_chain_ref.clone()),
            },
        )
        .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;

    match root_status {
        TaskStatus::Failed if requested_agent_ids.is_empty() => manager
            .resume_tree(chain.root_task_id.as_str())
            .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?,
        TaskStatus::Failed => {
            task_store
                .start_failed_root_for_recovery(&chain.root_task_id)
                .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;
        }
        TaskStatus::Running => {}
        other => {
            return Err(ApiError::InvalidInput(format!(
                "当前执行链状态不支持继续: {other:?}"
            )));
        }
    }
    for branch in &branches_to_resume {
        release_resumed_branch_path(task_store, &chain, branch)
            .map_err(|msg| ApiError::internal_assembly("继续会话失败", msg))?;
    }

    // 必须在启动 runner 前切换 current turn。旧 interrupted turn 仍保留在历史中，
    // 新 runner 的所有流式/工具写回都绑定到这个新 turn，避免首个事件落到旧轮次。
    let prepared_turn = prepare_turn(
        &chain,
        primary_branch,
        &branches_to_resume,
        &coordinator_attempt,
    )?;

    // 旧 runner 已在恢复状态前完成退出；这里只允许启动一个全新的执行轮。
    match manager.start_after_quiesce(chain.root_task_id.as_str(), Some(session_id.clone())) {
        Ok(_) => {}
        Err(RunnerStartError::AlreadyRunning) => {
            return Err(ApiError::internal_assembly(
                "继续会话失败",
                "恢复锁内仍存在活动 runner",
            ));
        }
        Err(RunnerStartError::NotFound) => {
            return Err(ApiError::internal_assembly("继续会话失败", "根任务不存在"));
        }
        Err(RunnerStartError::SessionUnavailable) => {
            return Err(ApiError::InvalidInput(
                "当前会话已关闭，不能继续执行".to_string(),
            ));
        }
    }

    if let Some(commit) = recovery_commit
        && let Err(error) = commit_chain_recovery(
            &state.session_store,
            &state.workspace_registry,
            session_id,
            commit,
        )
    {
        manager
            .quiesce_for_restart(chain.root_task_id.as_str())
            .await;
        let message = error.into_message();
        return Err(ApiError::internal_assembly("提交恢复状态失败", message));
    }

    goal_resume_guard.commit();
    let accepted = SessionContinueAccepted {
        session_id: session_id.clone(),
        mission_id: chain.mission_id,
        root_task_id: chain.root_task_id,
        action_task_id: primary_branch.task_id.clone(),
        turn_id: resumed_turn_id.to_string(),
        execution_chain_ref: chain.execution_chain_ref,
        resumed_branch_count: branches_to_resume.len(),
        runner_started: true,
    };
    recovery_claim.commit();
    recovery_attempt.commit();
    Ok((accepted, prepared_turn))
}

#[cfg(test)]
mod tests {
    use magi_session_store::{CanonicalTurn, SESSION_COMMAND_METADATA_KEY};
    use magi_session_store::{ExecutionThread, SessionStore, SessionStoreState};

    fn canonical_turn(
        turn_id: &str,
        turn_seq: u64,
        user_text: &str,
        command: bool,
    ) -> CanonicalTurn {
        let mut user_metadata = serde_json::Map::new();
        if command {
            user_metadata.insert(
                SESSION_COMMAND_METADATA_KEY.to_string(),
                serde_json::json!("compact"),
            );
        }
        serde_json::from_value(serde_json::json!({
            "sessionId": "session-command-history",
            "turnId": turn_id,
            "turnSeq": turn_seq,
            "acceptedAt": turn_seq,
            "status": "completed",
            "items": [{
                "sessionId": "session-command-history",
                "turnId": turn_id,
                "turnSeq": turn_seq,
                "itemId": format!("{turn_id}-user"),
                "itemSeq": 1,
                "kind": "user_message",
                "createdAt": turn_seq,
                "status": "completed",
                "updatedAt": turn_seq,
                "content": user_text,
                "sourceThreadId": "thread-orchestrator",
                "metadata": user_metadata,
            }],
        }))
        .expect("canonical turn fixture should deserialize")
    }

    #[test]
    fn session_command_turns_never_enter_rebuilt_model_history() {
        let turns = vec![
            canonical_turn("turn-plain", 1, "实现登录接口", false),
            canonical_turn("turn-compact", 2, "保留接口约束", true),
        ];
        assert!(turns[1].is_session_command());
        let thread: ExecutionThread = serde_json::from_value(serde_json::json!({
            "threadId": "thread-orchestrator", "sessionId": "session-command-history",
            "missionId": "mission-history", "roleId": "orchestrator", "workerInstanceId": "worker-history",
            "status": "idle", "createdAt": 1, "lastUsedAt": 2,
        })).unwrap();
        let store = SessionStore::from_state_for_test(SessionStoreState {
            canonical_turns: turns,
            ..Default::default()
        });
        let history = store.canonical_thread_message_history(&thread).unwrap();
        let contents = history
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(contents, vec!["实现登录接口"]);
    }
    #[test]
    fn continue_history_only_reaches_frozen_recipients_after_reload() {
        let mut turn = canonical_turn("turn-continue", 1, "继续处理所选任务", false);
        turn.items[0]
            .metadata
            .insert("route".into(), serde_json::json!("continue"));
        turn.items[0]
            .metadata
            .insert("continuedThreadIds".into(), serde_json::json!(["selected"]));
        let threads = ["selected", "unselected", "created-later"].map(|id| {
            serde_json::from_value::<ExecutionThread>(serde_json::json!({
                "threadId": id, "sessionId": "session-command-history", "missionId": "mission-history",
                "roleId": "executor", "workerInstanceId": format!("worker-{id}"),
                "status": "idle", "createdAt": 1, "lastUsedAt": 2,
            })).unwrap()
        });
        let store = SessionStore::from_state_for_test(SessionStoreState {
            canonical_turns: vec![turn.clone()],
            thread_registry: threads.to_vec(),
            ..Default::default()
        });
        for store in [
            store.clone(),
            SessionStore::from_state_for_test(store.export_state()),
        ] {
            for thread in &threads {
                let snapshot = store.thread_context_snapshot(&thread.thread_id).unwrap();
                assert_eq!(
                    snapshot.messages().len(),
                    usize::from(thread.thread_id.as_str() == "selected")
                );
            }
        }
        // 没有明确接收范围的旧 Continue 不能广播到任意线程。
        turn.items[0].metadata.remove("continuedThreadIds");
        let store = SessionStore::from_state_for_test(SessionStoreState {
            canonical_turns: vec![turn],
            thread_registry: threads.to_vec(),
            ..Default::default()
        });
        for thread in &threads {
            assert!(
                store
                    .thread_context_snapshot(&thread.thread_id)
                    .unwrap()
                    .messages()
                    .is_empty()
            );
        }
    }
}
