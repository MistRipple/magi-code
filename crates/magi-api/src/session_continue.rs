//! 继续会话 API 适配层。
//!
//! 纯判定、校验、writeback 落盘、branch checkpoint 同步和子树解封逻辑已下沉到
//! `magi_conversation_runtime::execution_chain_recovery`；本模块只负责把 `ApiState`
//! 持有的 runner、task store 与 execution registry 装配给 runtime 恢复流程。

use crate::{
    errors::ApiError,
    state::{ApiState, RunnerStartError},
};
use magi_conversation_runtime::{
    execution_chain_recovery::{
        apply_chain_recovery_if_needed, commit_chain_recovery, fail_resumed_execution_paths,
        release_resumed_branch_path, sync_branch_checkpoint_to_worker_runtime,
    },
    session_images::SessionTurnImage,
    task_execution_registry::TaskExecutionPlan,
};
use magi_core::{
    ExecutionOwnership, SessionId, SessionLifecycleStatus, TaskExecutionTarget, TaskStatus,
    ThreadId, UtcMillis, WorkerId,
};
use magi_orchestrator::ExecutionWritebackPlans;
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, CanonicalTurn, CanonicalTurnItemKind,
    ExecutionThread, ExecutionThreadStatus, InterruptedGoalResumeCheckpoint, SessionStore,
    ThreadChatImageSource, ThreadChatMessage, ThreadChatToolCall, ThreadChatToolFunction,
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

struct SessionGitExecutionLeaseGuard<'a> {
    state: &'a ApiState,
    session_id: &'a SessionId,
    committed: bool,
}

impl<'a> SessionGitExecutionLeaseGuard<'a> {
    fn new(state: &'a ApiState, session_id: &'a SessionId) -> Self {
        Self {
            state,
            session_id,
            committed: false,
        }
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for SessionGitExecutionLeaseGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.state
                .release_session_git_execution_lease(self.session_id);
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
    ) -> Self {
        Self {
            state,
            session_id: session_id.clone(),
            chain: chain.clone(),
            branches: branches.to_vec(),
            resumed_turn_id: resumed_turn_id.to_string(),
            registered_task_ids: Vec::new(),
            registered_threads: Vec::new(),
            committed: false,
        }
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
            && let Err(error) = fail_resumed_execution_paths(
                task_store,
                self.state.spawn_graph.as_ref(),
                &self.chain,
                &self.branches,
            )
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
        if let Err(error) = self.state.persist_runtime_durable_state_for_api() {
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
    finalize_terminal_worker_branches, task_status_is_terminal,
};

/// 新 Turn 已经写入后，如果 runner 启动失败，必须把它收口为失败态。
///
/// Continue 入口会先持久化用户可见的 running Turn，再启动后台 runner；启动阶段的
/// 任何失败都不能把这个 Turn 留在 running，否则下一次发送会继续看到一个永远不会
/// 收口的“活动会话”，并再次触发旧执行链的恢复逻辑。
fn fail_prepared_continue_turn(state: &ApiState, session_id: &SessionId, turn_id: &str) {
    match state.session_store.update_current_turn_status_for_turn(
        session_id,
        Some(turn_id),
        "failed",
    ) {
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
    turn_id: &str,
    execution_root: Option<std::path::PathBuf>,
    execution_settings_snapshot: Option<Arc<SettingsStore>>,
) -> TaskExecutionPlan {
    let ownership = ExecutionOwnership {
        session_id: Some(chain.session_id.clone()),
        workspace_id: chain.workspace_id.clone(),
        mission_id: Some(chain.mission_id.clone()),
        task_id: Some(branch.task_id.clone()),
        worker_id: Some(branch.worker_id.clone()),
        execution_chain_ref: Some(chain.execution_chain_ref.clone()),
    };
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
            recovery_id: chain.recovery_ref.clone(),
            execution_chain_ref: Some(chain.execution_chain_ref.clone()),
        },
        worker_id: branch.worker_id.clone(),
        thread_id: branch.thread_id.clone(),
        is_primary: branch.is_primary,
        session_id: chain.session_id.clone(),
        turn_id: turn_id.to_string(),
        workspace_id: chain.workspace_id.clone(),
        execution_root,
        ownership,
        writebacks,
        use_tools: branch.use_tools,
        skill_name: branch.skill_name.clone(),
        images: Vec::new(),
        execution_settings_snapshot,
    }
}

fn canonical_value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        _ => value.to_string(),
    }
}

fn rebuild_thread_history_from_canonical(
    turns: &[CanonicalTurn],
    thread_id: &ThreadId,
) -> Vec<ThreadChatMessage> {
    let mut history = Vec::new();
    for item in turns
        .iter()
        .flat_map(|turn| turn.items.iter())
        .filter(|item| &item.source_thread_id == thread_id)
    {
        match item.kind {
            CanonicalTurnItemKind::UserMessage => {
                if item
                    .content
                    .as_deref()
                    .is_some_and(|content| !content.trim().is_empty())
                {
                    history.push(ThreadChatMessage {
                        role: "user".to_string(),
                        content: item.content.clone(),
                        images: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        provider_context: Vec::new(),
                    });
                }
            }
            CanonicalTurnItemKind::AssistantText => {
                if item
                    .content
                    .as_deref()
                    .is_some_and(|content| !content.trim().is_empty())
                {
                    history.push(ThreadChatMessage {
                        role: "assistant".to_string(),
                        content: item.content.clone(),
                        images: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        provider_context: Vec::new(),
                    });
                }
            }
            CanonicalTurnItemKind::ToolCall => {
                let Some(tool) = item.tool.as_ref() else {
                    continue;
                };
                let arguments = tool
                    .arguments
                    .as_ref()
                    .map(canonical_value_text)
                    .unwrap_or_else(|| "{}".to_string());
                history.push(ThreadChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    images: Vec::new(),
                    tool_calls: vec![ThreadChatToolCall {
                        id: tool.call_id.clone(),
                        kind: "function".to_string(),
                        function: ThreadChatToolFunction {
                            name: tool.name.clone(),
                            arguments,
                        },
                    }],
                    tool_call_id: None,
                    provider_context: Vec::new(),
                });
                let result = tool
                    .result
                    .as_ref()
                    .map(canonical_value_text)
                    .or_else(|| {
                        tool.error.as_ref().map(|error| {
                            serde_json::json!({
                                "tool": tool.name,
                                "status": "failed",
                                "error": error,
                            })
                            .to_string()
                        })
                    })
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "tool": tool.name,
                            "status": "interrupted",
                            "reason": "legacy_thread_history_rebuilt_without_result",
                        })
                        .to_string()
                    });
                history.push(ThreadChatMessage {
                    role: "tool".to_string(),
                    content: Some(result),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(tool.call_id.clone()),
                    provider_context: Vec::new(),
                });
            }
            CanonicalTurnItemKind::AssistantThinking
            | CanonicalTurnItemKind::TaskStatus
            | CanonicalTurnItemKind::SystemNotice => {}
        }
    }
    history
}

pub(crate) fn restore_missing_resumed_branch_threads(
    state: &ApiState,
    session_id: &SessionId,
    chain: &ActiveExecutionChain,
    branches: &[ActiveExecutionBranch],
) -> Result<Vec<ExecutionThread>, ApiError> {
    let registered_threads = state.session_store.thread_registry_snapshot(session_id);
    let missing_branches = branches
        .iter()
        .filter(|branch| {
            !registered_threads
                .iter()
                .any(|thread| thread.thread_id == branch.thread_id)
        })
        .collect::<Vec<_>>();
    if missing_branches.is_empty() {
        return Ok(Vec::new());
    }

    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("继续会话失败", "task_store 未配置"))?;
    let canonical_turns = state.session_store.canonical_turns_for_session(session_id);
    let mut threads_to_register = Vec::with_capacity(missing_branches.len());
    for branch in &missing_branches {
        let task = task_store.get_task(&branch.task_id).ok_or_else(|| {
            ApiError::not_found("恢复 branch 任务不存在", branch.task_id.as_str())
        })?;
        if task.mission_id != chain.mission_id || task.root_task_id != chain.root_task_id {
            return Err(ApiError::internal_assembly(
                "继续会话失败",
                format!("恢复 branch 不属于当前执行链: {}", branch.task_id),
            ));
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
            message_history: rebuild_thread_history_from_canonical(
                &canonical_turns,
                &branch.thread_id,
            ),
        };
        if threads_to_register
            .iter()
            .any(|existing: &ExecutionThread| existing.thread_id == thread.thread_id)
        {
            return Err(ApiError::internal_assembly(
                "继续会话失败",
                format!("恢复分支重复使用 thread: {}", thread.thread_id),
            ));
        }
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
    if let Err(error) = state.persist_session_state_checkpoint("session_continue_thread_rebuild") {
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

pub(crate) fn persist_resumed_branch_user_input(
    state: &ApiState,
    session_id: &SessionId,
    branches: &[ActiveExecutionBranch],
    prompt_text: Option<&str>,
    images: &[SessionTurnImage],
    accepted_at: UtcMillis,
) -> Result<(), ApiError> {
    if prompt_text.is_none() && images.is_empty() {
        return Ok(());
    }

    let mut thread_ids = Vec::new();
    for branch in branches {
        if !thread_ids.contains(&branch.thread_id) {
            thread_ids.push(branch.thread_id.clone());
        }
    }
    let registered_threads = state.session_store.thread_registry_snapshot(session_id);
    if let Some(missing_thread_id) = thread_ids.iter().find(|thread_id| {
        !registered_threads
            .iter()
            .any(|thread| thread.thread_id == **thread_id)
    }) {
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            format!("恢复分支 thread 不存在: {missing_thread_id}"),
        ));
    }

    let message = ThreadChatMessage {
        role: "user".to_string(),
        content: prompt_text.map(str::to_string),
        images: images
            .iter()
            .map(|image| ThreadChatImageSource {
                kind: image.source.kind.clone(),
                media_type: image.source.media_type.clone(),
                data: image.source.data.clone(),
            })
            .collect(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_context: Vec::new(),
    };
    for thread_id in thread_ids {
        state
            .session_store
            .append_thread_messages(&thread_id, vec![message.clone()], accepted_at);
    }
    state.persist_session_state_checkpoint("session_continue_task_input")?;
    Ok(())
}

/// 在持有 session 生命周期锁且领取恢复权后、启动新 runner 前写入本轮用户信号。
///
/// 这使“用户输入接管中断任务”和“启动恢复 runner”属于同一个串行临界区，避免双击
/// 恢复链接或多个窗口同时提交时把第二条输入遗留到错误执行链中。
pub(crate) async fn continue_execution_chain_with_pre_resume<T, U, F, G>(
    state: &ApiState,
    session_id: &SessionId,
    requested_agent_ids: &[WorkerId],
    resumed_turn_id: &str,
    resumed_at: UtcMillis,
    prepare_input: F,
    prepare_turn: G,
) -> Result<(SessionContinueAccepted, T, U), ApiError>
where
    F: FnOnce(&[ActiveExecutionBranch]) -> Result<T, ApiError>,
    G: FnOnce(&ActiveExecutionChain, &ActiveExecutionBranch, &T) -> Result<U, ApiError>,
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
    manager
        .quiesce_for_restart(chain.root_task_id.as_str())
        .await;

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
    let worker_runtime_handle = state
        .execution_pipeline()
        .map(|pipeline| pipeline.execution_runtime.worker_runtime());
    finalize_terminal_worker_branches(
        &state.session_store,
        Some(task_store),
        worker_runtime_handle,
        session_id,
    )
    .map_err(|msg| ApiError::internal_assembly("收敛代理终态失败", msg))?;
    let resumable_branches = chain
        .branches
        .iter()
        .filter(|&branch| {
            active_execution_branch_is_continue_recoverable(
                worker_runtime_handle,
                state.task_store(),
                &chain,
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
    let execution_root = if workspace_id.is_none() {
        Some(state.personal_session_execution_root(session_id)?)
    } else {
        None
    };
    state
        .ensure_snapshot_session_for_workspace_id(session_id, &workspace_id)
        .await?;
    state
        .ensure_session_code_context(session_id, &workspace_id)
        .await?;
    let git_execution_lease = SessionGitExecutionLeaseGuard::new(state, session_id);

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
    if let Some(previous_turn_id) = chain
        .current_turn
        .as_ref()
        .map(|turn| turn.turn_id.as_str())
    {
        state
            .session_store
            .finalize_current_turn_for_continue(session_id, previous_turn_id)
            .map_err(|error| ApiError::internal_assembly("收口 Continue 前置 Turn 失败", error))?;
        state.persist_session_state_checkpoint("session_continue_finalize_previous_turn")?;
    }

    let recovery_claim = InterruptedRecoveryClaimGuard::claim(&state.session_store, session_id)?;
    let mut recovery_attempt = ContinueRecoveryAttempt::new(
        state,
        session_id,
        &chain,
        &branches_to_resume,
        resumed_turn_id,
    );
    for thread in
        restore_missing_resumed_branch_threads(state, session_id, &chain, &branches_to_resume)?
    {
        recovery_attempt.record_registered_thread(thread);
    }
    let prepared_input = prepare_input(&branches_to_resume)?;
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
        if let Some(worker_runtime) = worker_runtime_handle {
            sync_branch_checkpoint_to_worker_runtime(worker_runtime, branch);
        }
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
                recovery_id: chain.recovery_ref.clone(),
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
        release_resumed_branch_path(task_store, state.spawn_graph.as_ref(), &chain, branch)
            .map_err(|msg| ApiError::internal_assembly("继续会话失败", msg))?;
    }

    // 必须在启动 runner 前切换 current turn。旧 interrupted turn 仍保留在历史中，
    // 新 runner 的所有流式/工具写回都绑定到这个新 turn，避免首个事件落到旧轮次。
    let prepared_turn = prepare_turn(&chain, primary_branch, &prepared_input)?;

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
            &mut chain,
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
    git_execution_lease.commit();
    recovery_attempt.commit();
    Ok((accepted, prepared_input, prepared_turn))
}
