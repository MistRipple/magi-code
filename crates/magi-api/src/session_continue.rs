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
        apply_chain_recovery_if_needed, release_resumed_branch_path,
        sync_branch_checkpoint_to_worker_runtime,
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
    ExecutionThread, ExecutionThreadStatus, SessionStore, ThreadChatImageSource, ThreadChatMessage,
    ThreadChatToolCall, ThreadChatToolFunction,
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

// 对 routes/sessions.rs 暴露继续会话所需的 runtime 数据载体与判定函数。
pub(crate) use magi_conversation_runtime::execution_chain_recovery::{
    SessionContinueAccepted, active_execution_branch_is_continue_recoverable,
    finalize_terminal_worker_branches, task_status_is_terminal,
};

fn rebuild_dispatch_plan_for_branch(
    chain: &ActiveExecutionChain,
    branch: &ActiveExecutionBranch,
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
        workspace_id: chain.workspace_id.clone(),
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
) -> Result<usize, ApiError> {
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
        return Ok(0);
    }

    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("继续会话失败", "task_store 未配置"))?;
    let canonical_turns = state.session_store.canonical_turns_for_session(session_id);
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
        state.session_store.register_thread(ExecutionThread {
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
        });
    }
    state.persist_session_state_checkpoint("session_continue_thread_rebuild")?;
    Ok(missing_branches.len())
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
pub(crate) async fn continue_execution_chain_with_pre_resume<T, F>(
    state: &ApiState,
    session_id: &SessionId,
    requested_agent_ids: &[WorkerId],
    prepare_input: F,
) -> Result<(SessionContinueAccepted, T), ApiError>
where
    F: FnOnce(&[ActiveExecutionBranch]) -> Result<T, ApiError>,
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
    let _session_lifecycle_guard = manager.lock_session_lifecycle(session_id).await;
    let session = state
        .session_store
        .session(session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    if session.status != SessionLifecycleStatus::Active {
        return Err(ApiError::InvalidInput(
            "当前会话已关闭，不能继续执行".to_string(),
        ));
    }
    let worker_runtime_handle = state
        .execution_pipeline()
        .map(|pipeline| pipeline.execution_runtime.worker_runtime());
    finalize_terminal_worker_branches(
        &state.session_store,
        state.task_store(),
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
    if resumable_branches.is_empty() {
        return Err(ApiError::InvalidInput(
            "当前会话没有可继续的 branch".to_string(),
        ));
    }
    if !requested_agent_ids.is_empty() {
        for agent_id in requested_agent_ids {
            if !chain
                .branches
                .iter()
                .any(|branch| &branch.worker_id == agent_id)
            {
                return Err(ApiError::InvalidInput(format!(
                    "请求继续的代理不属于当前执行链: {}",
                    agent_id
                )));
            }
        }
        let has_requested_resumable_agent = requested_agent_ids.iter().any(|agent_id| {
            resumable_branches
                .iter()
                .any(|branch| &branch.worker_id == agent_id)
        });
        if !has_requested_resumable_agent {
            return Err(ApiError::InvalidInput(
                "请求继续的代理当前不可继续".to_string(),
            ));
        }
    }

    let branches_to_resume = if requested_agent_ids.is_empty() {
        resumable_branches.clone()
    } else {
        resumable_branches
            .iter()
            .filter(|branch| {
                requested_agent_ids
                    .iter()
                    .any(|agent_id| agent_id == &branch.worker_id)
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    if branches_to_resume.is_empty() {
        return Err(ApiError::InvalidInput(
            "请求继续的代理当前不可继续".to_string(),
        ));
    }

    let workspace_id = state
        .session_workspace_id(&session)
        .or_else(|| chain.workspace_id.clone());
    state
        .ensure_snapshot_session_for_workspace_id(session_id, &workspace_id)
        .await?;
    state
        .ensure_session_code_context(session_id, &workspace_id)
        .await?;
    let git_execution_lease = SessionGitExecutionLeaseGuard::new(state, session_id);

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
    apply_chain_recovery_if_needed(
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

    let recovery_claim = InterruptedRecoveryClaimGuard::claim(&state.session_store, session_id)?;
    restore_missing_resumed_branch_threads(state, session_id, &chain, &branches_to_resume)?;
    let prepared_input = prepare_input(&branches_to_resume)?;

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
            .update_status(&chain.root_task_id, TaskStatus::Failed)
            .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;
        root_status = TaskStatus::Failed;
    } else if task_status_is_terminal(&root_status) {
        return Err(ApiError::InvalidInput(
            "当前会话执行链已结束，不能继续".to_string(),
        ));
    }

    let execution_settings_snapshot = Some(Arc::new(state.settings_store.execution_snapshot()));
    for branch in &branches_to_resume {
        state.task_execution_registry().insert(
            branch.task_id.clone(),
            rebuild_dispatch_plan_for_branch(&chain, branch, execution_settings_snapshot.clone()),
        );
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
                .update_status(&chain.root_task_id, TaskStatus::Running)
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

    let accepted = SessionContinueAccepted {
        session_id: session_id.clone(),
        mission_id: chain.mission_id,
        root_task_id: chain.root_task_id,
        action_task_id: primary_branch.task_id.clone(),
        execution_chain_ref: chain.execution_chain_ref,
        resumed_branch_count: branches_to_resume.len(),
        runner_started: true,
    };
    recovery_claim.commit();
    git_execution_lease.commit();
    Ok((accepted, prepared_input))
}
