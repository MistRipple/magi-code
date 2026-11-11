//! 任务系统 — 执行链"继续会话"恢复路径。
//!
//! 本文件承担数据载体（[`SessionContinueAccepted`]）+ 纯/弱状态判定 + recovery
//! 校验 / 校对 / 应用 / writeback 落盘 / branch checkpoint 同步 / 子树解封等
//! "继续会话"实现细节。错误类型统一为 `String`，函数签名走显式 stores。

#[cfg(test)]
use magi_core::UtcMillis;
use magi_core::{ExecutionOwnership, RecoveryResumeInput, SessionId, TaskStatus};
use magi_memory_store::MemoryStore;
use magi_orchestrator::{ExecutionWritebackPlans, task_store::TaskStore};
use magi_session_store::{ActiveExecutionBranch, ActiveExecutionChain, SessionStore};
use magi_workspace::{RecoveryStatus, WorkspaceStore};

#[derive(Clone, Debug)]
pub struct SessionContinueAccepted {
    pub session_id: SessionId,
    pub mission_id: magi_core::MissionId,
    pub root_task_id: magi_core::TaskId,
    pub action_task_id: magi_core::TaskId,
    pub turn_id: String,
    pub execution_chain_ref: String,
    pub resumed_branch_count: usize,
    pub runner_started: bool,
}

pub fn task_status_is_terminal(status: &TaskStatus) -> bool {
    matches!(status, TaskStatus::Completed | TaskStatus::Killed)
}

fn task_status_is_continue_recoverable(status: &TaskStatus) -> bool {
    matches!(status, TaskStatus::Failed)
}

fn root_task_allows_continue(status: &TaskStatus) -> bool {
    !matches!(status, TaskStatus::Completed | TaskStatus::Killed)
}

pub fn active_execution_branch_is_continue_recoverable(
    task_store: Option<&TaskStore>,
    chain: &ActiveExecutionChain,
    current_turn: Option<&magi_session_store::ActiveExecutionTurn>,
    branch: &ActiveExecutionBranch,
) -> bool {
    // 当前 Turn 是 canonical 的只读投影。插件命令没有模型检查点，不能走通用
    // Continue 重放副作用；用户须核对结果后重新发起明确的命令。
    if chain.current_turn_id.as_deref() != current_turn.map(|turn| turn.turn_id.as_str()) {
        return false;
    }
    if current_turn.is_some_and(|turn| {
        turn.items.iter().any(|item| {
            item.kind == "user_message" && item.metadata.contains_key("pluginCommandId")
        })
    }) {
        return false;
    }
    let Some(task_store) = task_store else {
        return false;
    };
    let Some(root_task) = task_store.get_task(&chain.root_task_id) else {
        return false;
    };
    if root_task.mission_id != chain.mission_id || !root_task_allows_continue(&root_task.status) {
        return false;
    }
    let Some(task) = task_store.get_task(&branch.task_id) else {
        return false;
    };
    task.mission_id == chain.mission_id
        && task.root_task_id == chain.root_task_id
        && task_store
            .execution_scope_root(&task.task_id)
            .is_ok_and(|scope| scope == chain.root_task_id)
        && task_status_is_continue_recoverable(&task.status)
}

/// 沿 TaskStore 的父任务链上溯，把恢复 branch 与其祖先链上的可恢复 Failed 任务恢复为 Pending。
pub fn release_resumed_branch_path(
    task_store: &TaskStore,
    chain: &ActiveExecutionChain,
    branch: &ActiveExecutionBranch,
) -> Result<(), String> {
    if task_store
        .execution_scope_root(&branch.task_id)
        .map_err(|error| error.to_string())?
        != chain.root_task_id
    {
        return Err("恢复 branch 已转交其他拥有方，原主线不能重新打开".into());
    }
    let mut current_task_id = Some(branch.task_id.clone());
    while let Some(task_id) = current_task_id {
        if task_id == chain.root_task_id {
            break;
        }
        let task = task_store
            .get_task(&task_id)
            .ok_or_else(|| format!("继续 branch 任务不存在: {task_id}"))?;
        if task.mission_id != chain.mission_id || task.root_task_id != chain.root_task_id {
            return Err(format!("branch 路径任务不属于当前执行链: {task_id}"));
        }
        current_task_id = task.parent_task_id.clone();
        if task.status == TaskStatus::Failed {
            task_store
                .reopen_failed_task_for_recovery(&task_id)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// 将一次恢复尝试触及的 branch 路径收口为 Failed。
///
/// 该函数只使用当前仍然有效的 lease 做撤销；没有 lease 的 Pending/Running 任务可以
/// 被标记为 Failed，但绝不允许借此伪造 Completed。它是 Continue 的失败回滚边界，重复
/// 调用对已终态任务幂等。
pub fn fail_resumed_execution_paths(
    task_store: &TaskStore,
    chain: &ActiveExecutionChain,
    branches: &[ActiveExecutionBranch],
) -> Result<(), String> {
    let mut task_ids = Vec::new();
    for branch in branches {
        if task_store
            .execution_scope_root(&branch.task_id)
            .map_err(|error| error.to_string())?
            != chain.root_task_id
        {
            // 旧 Continue 在失败回滚前也可能失去子树；只回滚仍归原主线
            // 的路径，不能重置、取消或重新激活已转交代理。
            continue;
        }
        let mut path_seen = Vec::new();
        let mut current_task_id = Some(branch.task_id.clone());
        while let Some(task_id) = current_task_id {
            if path_seen.iter().any(|seen| seen == &task_id) {
                return Err(format!("恢复 branch {} 的父任务路径存在环", branch.task_id));
            }
            path_seen.push(task_id.clone());
            if !task_ids.iter().any(|seen| seen == &task_id) {
                task_ids.push(task_id.clone());
            }
            if task_id == chain.root_task_id {
                break;
            }
            let task = task_store
                .get_task(&task_id)
                .ok_or_else(|| format!("恢复 branch 任务不存在: {task_id}"))?;
            if task.mission_id != chain.mission_id || task.root_task_id != chain.root_task_id {
                return Err(format!("恢复 branch 路径任务不属于当前执行链: {task_id}"));
            }
            current_task_id = task.parent_task_id.clone();
        }
    }
    if !task_ids
        .iter()
        .any(|task_id| task_id == &chain.root_task_id)
    {
        task_ids.push(chain.root_task_id.clone());
    }

    for task_id in task_ids {
        let task = task_store
            .get_task(&task_id)
            .ok_or_else(|| format!("恢复失败收口任务不存在: {task_id}"))?;
        if task_status_is_terminal(&task.status) || task.status == TaskStatus::Failed {
            continue;
        }
        if !matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
            return Err(format!(
                "恢复失败收口任务 {} 当前状态 {:?} 不可写入 Failed",
                task_id, task.status
            ));
        }
        let active_lease = task_store.get_active_lease(&task_id);
        let changed = task_store
            .revoke_lease_and_set_task_terminal(
                &task_id,
                &chain.root_task_id,
                active_lease.as_ref().map(|lease| &lease.lease_id),
                TaskStatus::Failed,
                Vec::new(),
            )
            .map_err(|error| error.to_string())?;
        if !changed {
            let current = task_store
                .get_task(&task_id)
                .ok_or_else(|| format!("恢复失败收口任务不存在: {task_id}"))?;
            if !matches!(
                current.status,
                TaskStatus::Failed | TaskStatus::Completed | TaskStatus::Killed
            ) {
                return Err(format!(
                    "恢复失败收口任务 {} 未提交 Failed，当前状态 {:?}",
                    task_id, current.status
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
pub enum RecoveryValidationError {
    /// recovery_id 在 workspace_registry 里查不到。
    NotFound { recovery_id: String },
    /// recovery 当前状态不允许继续。
    InvalidStatus { message: String },
    /// recovery 输入构建失败或入口与当前 chain 不匹配。
    Mismatch { message: String },
    /// 其它装配性错误（writebacks 落盘 / session_store 写入失败等）。
    Internal { message: String },
}

impl RecoveryValidationError {
    pub fn into_message(self) -> String {
        match self {
            Self::NotFound { recovery_id } => format!("recovery 不存在: {recovery_id}"),
            Self::InvalidStatus { message } => message,
            Self::Mismatch { message } => message,
            Self::Internal { message } => message,
        }
    }
}

pub fn validate_recovery_status(
    workspace_registry: &WorkspaceStore,
    recovery_id: &str,
) -> Result<(), RecoveryValidationError> {
    let export = workspace_registry
        .recovery_sidecar_export(recovery_id)
        .ok_or_else(|| RecoveryValidationError::NotFound {
            recovery_id: recovery_id.to_string(),
        })?;
    match export.current_status {
        RecoveryStatus::Ready => Ok(()),
        RecoveryStatus::Prepared => Err(RecoveryValidationError::InvalidStatus {
            message: format!(
                "继续检查点 {} 当前状态为 prepared，必须先进入 ready 才能继续会话",
                recovery_id
            ),
        }),
        RecoveryStatus::Consumed => Err(RecoveryValidationError::InvalidStatus {
            message: format!("继续检查点 {} 已被消费，不能再次继续会话", recovery_id),
        }),
    }
}

pub fn map_recovery_input_error(
    recovery_id: &str,
    error: magi_core::DomainError,
) -> RecoveryValidationError {
    match error {
        magi_core::DomainError::NotFound { .. } => RecoveryValidationError::NotFound {
            recovery_id: recovery_id.to_string(),
        },
        magi_core::DomainError::InvalidState { message }
        | magi_core::DomainError::GoalRejected { message, .. }
        | magi_core::DomainError::RunSafetyLimit { message }
        | magi_core::DomainError::Validation { message } => {
            RecoveryValidationError::Mismatch { message }
        }
        magi_core::DomainError::AlreadyExists { entity } => RecoveryValidationError::Internal {
            message: format!("recovery 输入构建遇到重复实体: {entity}"),
        },
        magi_core::DomainError::Persistence { message } => {
            RecoveryValidationError::Internal { message }
        }
        magi_core::DomainError::CurrentTurnConflict {
            session_id,
            active_turn_id,
        } => RecoveryValidationError::Internal {
            message: format!(
                "recovery 输入构建遇到无关的会话轮次冲突: session={session_id}, turn={active_turn_id}"
            ),
        },
    }
}

pub fn validate_recovery_input_matches_chain(
    chain: &ActiveExecutionChain,
    input: &RecoveryResumeInput,
) -> Result<(), RecoveryValidationError> {
    if input.ownership.session_id.as_ref() != Some(&chain.session_id) {
        return Err(RecoveryValidationError::Mismatch {
            message: format!(
                "恢复入口 {} 不属于当前会话 {}",
                input.recovery_id, chain.session_id
            ),
        });
    }
    if input.ownership.mission_id.as_ref() != Some(&chain.mission_id) {
        return Err(RecoveryValidationError::Mismatch {
            message: format!(
                "恢复入口 {} 不属于当前执行链 mission {}",
                input.recovery_id, chain.mission_id
            ),
        });
    }
    if input.ownership.workspace_id != chain.workspace_id {
        return Err(RecoveryValidationError::Mismatch {
            message: format!("恢复入口 {} 的工作区与当前执行链不一致", input.recovery_id),
        });
    }
    if input.ownership.execution_chain_ref.as_deref() != Some(chain.execution_chain_ref.as_str()) {
        return Err(RecoveryValidationError::Mismatch {
            message: format!(
                "恢复入口 {} 的 execution_chain_ref 与当前执行链不一致",
                input.recovery_id
            ),
        });
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ChainRecoveryCommit {
    recovery_id: String,
    ownership: ExecutionOwnership,
}

/// 若 Session 恢复引用存在：校验 recovery 状态 / 入口匹配 → 应用 recovery resume
/// 输入到 session_store → 落盘 writebacks。Recovery 仍保持 Ready，直到 runner 成功启动
/// 后由 [`commit_chain_recovery`] 完成消费和 Session 恢复引用清理。
pub fn apply_chain_recovery_if_needed(
    session_store: &SessionStore,
    workspace_registry: &WorkspaceStore,
    memory_store: Option<&MemoryStore>,
    session_id: &SessionId,
    chain: &ActiveExecutionChain,
    primary_branch: &ActiveExecutionBranch,
) -> Result<Option<ChainRecoveryCommit>, RecoveryValidationError> {
    let Some(recovery_id) = session_store.recovery_id(session_id) else {
        return Ok(None);
    };
    validate_recovery_status(workspace_registry, &recovery_id)?;
    let input = workspace_registry
        .build_recovery_resume_input(&recovery_id)
        .map_err(|error| map_recovery_input_error(&recovery_id, error))?;
    validate_recovery_input_matches_chain(chain, &input)?;

    session_store
        .apply_recovery_resume_input(session_id.clone(), input.clone())
        .map_err(|error| RecoveryValidationError::Internal {
            message: error.to_string(),
        })?;

    let writebacks = ExecutionWritebackPlans::from_continue_checkpoint_input(&input);
    if !writebacks.is_empty() {
        let memory_store = memory_store.ok_or_else(|| RecoveryValidationError::Internal {
            message: "execution pipeline 未配置".to_string(),
        })?;
        writebacks.apply(memory_store);
    }

    Ok(Some(ChainRecoveryCommit {
        recovery_id: input.recovery_id,
        ownership: ExecutionOwnership {
            session_id: Some(chain.session_id.clone()),
            workspace_id: chain.workspace_id.clone(),
            mission_id: Some(chain.mission_id.clone()),
            task_id: Some(primary_branch.task_id.clone()),
            worker_id: Some(primary_branch.worker_id.clone()),
            execution_chain_ref: Some(chain.execution_chain_ref.clone()),
        },
    }))
}

/// 在 runner 已成功启动后提交 recovery 消费。
///
/// 在 Session 提交锁内校验原恢复引用，再消费 Workspace 记录并清理引用。
/// 归属已变化时不消费；消费失败时保留引用，不尝试把已消费记录反向改回 Ready。
pub fn commit_chain_recovery(
    session_store: &SessionStore,
    workspace_registry: &WorkspaceStore,
    session_id: &SessionId,
    commit: ChainRecoveryCommit,
) -> Result<(), RecoveryValidationError> {
    session_store
        .consume_recovery_id(session_id, &commit.recovery_id, || {
            workspace_registry
                .consume_recovery_with_ownership(&commit.recovery_id, commit.ownership)
                .map(|_| ())
        })
        .map_err(|error| RecoveryValidationError::Internal {
            message: error.to_string(),
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{
        ExecutionOwnership, MissionId, Task, TaskId, TaskKind, TaskRuntimePayload, ThreadId,
        WorkerId, WorkspaceId,
    };
    use magi_session_store::{
        ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    };

    fn task(
        task_id: &str,
        root_task_id: &str,
        mission_id: &str,
        parent_task_id: Option<&str>,
        status: TaskStatus,
    ) -> Task {
        let now = UtcMillis(1);
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new(mission_id),
            root_task_id: TaskId::new(root_task_id),
            parent_task_id: parent_task_id.map(TaskId::new),
            kind: TaskKind::LocalAgent,
            title: task_id.to_string(),
            goal: task_id.to_string(),
            status,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: Default::default(),
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

    fn branch(task_id: &str, worker_id: &str) -> ActiveExecutionBranch {
        ActiveExecutionBranch {
            task_id: TaskId::new(task_id),
            worker_id: WorkerId::new(worker_id),
            use_tools: true,
            skill_name: None,
            is_primary: true,
            thread_id: ThreadId::new(format!("thread-{task_id}")),
        }
    }

    fn chain(
        session_id: &SessionId,
        mission_id: &MissionId,
        root_task_id: &TaskId,
        branch: ActiveExecutionBranch,
        _recovery_ref: Option<String>,
    ) -> ActiveExecutionChain {
        ActiveExecutionChain {
            session_id: session_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            execution_chain_ref: "chain-recovery-test".to_string(),
            workspace_id: Some(WorkspaceId::new("workspace-recovery-test")),
            branches: vec![branch],
            dispatch_context: ActiveExecutionDispatchContext {
                accepted_at: UtcMillis(1),
                entry_id: "entry-recovery-test".to_string(),
                trimmed_text: None,
                skill_name: None,
            },
            current_turn_id: None,
        }
    }

    #[test]
    fn mainline_continue_does_not_reopen_finalize_or_rollback_transferred_branch() {
        let store = TaskStore::new();
        let session = SessionId::new("transferred-continue-session");
        let mut root = task(
            "continue-root",
            "continue-root",
            "continue-mission",
            None,
            TaskStatus::Running,
        );
        root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
        store.insert_task(root.clone()).unwrap();
        let mut child = task(
            "continue-independent",
            "continue-root",
            "continue-mission",
            Some("continue-root"),
            TaskStatus::Pending,
        );
        let mut binding = magi_core::TaskExecutorBinding::for_role("executor");
        binding.agent_activation = Some(magi_core::AgentActivation {
            agent_id: child.task_id.clone(),
            session_id: session.clone(),
            lifetime: magi_core::AgentLifetime::Session,
            generation: 1,
            previous_task_id: None,
        });
        child.executor_binding = Some(binding);
        store.insert_task(child.clone()).unwrap();
        store
            .transfer_agent_subtree(
                &root.task_id,
                &child.task_id,
                magi_core::AgentRunTransfer {
                    operation_id: "continue-transfer".into(),
                    session_id: session.clone(),
                    source_turn_id: "source-turn".into(),
                    transferred_by: root.task_id.clone(),
                    activation_generation: 1,
                    owner: magi_core::AgentRunOwner::Session,
                    transferred_at: UtcMillis::now(),
                },
            )
            .unwrap();
        let branch = branch(child.task_id.as_str(), "independent-worker");
        let chain = chain(
            &session,
            &root.mission_id,
            &root.task_id,
            branch.clone(),
            None,
        );
        assert!(!active_execution_branch_is_continue_recoverable(
            Some(&store),
            &chain,
            None,
            &branch
        ));
        assert!(release_resumed_branch_path(&store, &chain, &branch).is_err());
        fail_resumed_execution_paths(&store, &chain, &[branch]).unwrap();
        assert_eq!(
            store.get_task(&child.task_id).unwrap().status,
            TaskStatus::Pending
        );
        assert_eq!(
            store.get_task(&root.task_id).unwrap().status,
            TaskStatus::Failed
        );
        assert_eq!(
            store.execution_scope_root(&child.task_id).unwrap(),
            child.task_id
        );
    }

    #[test]
    fn plugin_command_failure_requires_explicit_retry_instead_of_model_recovery() {
        let store = TaskStore::new();
        let session = SessionId::new("command-recovery");
        let root = task(
            "command-root",
            "command-root",
            "command-mission",
            None,
            TaskStatus::Failed,
        );
        store.insert_task(root.clone()).unwrap();
        let branch = branch(root.task_id.as_str(), "worker");
        let chain = chain(
            &session,
            &root.mission_id,
            &root.task_id,
            branch.clone(),
            None,
        );
        assert!(active_execution_branch_is_continue_recoverable(
            Some(&store),
            &chain,
            None,
            &branch
        ));
        let mut item = crate::session_writeback::session_turn_item(
            "user_message",
            "completed",
            None,
            Some("parameters".into()),
            None,
            branch.thread_id.clone(),
        );
        item.metadata.insert(
            "pluginCommandId".into(),
            serde_json::json!("plugin/acme.cmd/run"),
        );
        let current_turn = Some(magi_session_store::ActiveExecutionTurn {
            turn_id: "command-turn".into(),
            turn_seq: 1,
            accepted_at: UtcMillis(1),
            completed_at: Some(UtcMillis(2)),
            status: "failed".into(),
            user_message: Some("parameters".into()),
            items: vec![item],
        });
        let mut chain = chain;
        chain.current_turn_id = current_turn.as_ref().map(|turn| turn.turn_id.clone());
        assert!(!active_execution_branch_is_continue_recoverable(
            Some(&store),
            &chain,
            current_turn.as_ref(),
            &branch
        ));
        let reloaded = serde_json::from_value(serde_json::to_value(chain).unwrap()).unwrap();
        assert!(!active_execution_branch_is_continue_recoverable(
            Some(&store),
            &reloaded,
            current_turn.as_ref(),
            &branch
        ));
    }

    #[test]
    fn task_recovery_reads_task_status_without_stage_projection() {
        let store = TaskStore::new();
        let session = SessionId::new("session-finish-recovery");
        let root = task("root", "root", "mission", None, TaskStatus::Running);
        store.insert_task(root.clone()).unwrap();
        for status in [
            TaskStatus::Running,
            TaskStatus::Failed,
            TaskStatus::Completed,
            TaskStatus::Killed,
        ] {
            let child = task(
                &format!("child-{status:?}"),
                "root",
                "mission",
                Some("root"),
                status,
            );
            store.insert_task(child.clone()).unwrap();
            let branch = branch(child.task_id.as_str(), "worker");
            let chain = chain(
                &session,
                &root.mission_id,
                &root.task_id,
                branch.clone(),
                None,
            );
            assert_eq!(
                active_execution_branch_is_continue_recoverable(
                    Some(&store),
                    &chain,
                    None,
                    &branch
                ),
                status == TaskStatus::Failed,
                "恢复资格只能由 Task 的实际结果决定"
            );
            assert_eq!(
                serde_json::to_value(store.get_task(&child.task_id).unwrap()).unwrap(),
                serde_json::to_value(child).unwrap()
            );
        }
    }

    #[test]
    fn recovery_apply_stays_ready_until_runner_commit() {
        let session_store = SessionStore::new();
        let workspace_registry = WorkspaceStore::new();
        let session_id = SessionId::new("session-recovery-commit");
        let mission_id = MissionId::new("mission-recovery-commit");
        let root_task_id = TaskId::new("task-root-recovery-commit");
        let branch = branch("task-branch-recovery-commit", "worker-recovery-commit");
        let recovery_id = "recovery-commit-boundary";
        let chain_ref = "chain-recovery-test";
        session_store
            .create_session(session_id.clone(), "recovery commit")
            .expect("session should exist");
        workspace_registry.prepare_recovery_entry(
            WorkspaceId::new("workspace-recovery-test"),
            ExecutionOwnership {
                session_id: Some(session_id.clone()),
                mission_id: Some(mission_id.clone()),
                execution_chain_ref: Some(chain_ref.to_string()),
                ..ExecutionOwnership::default()
            },
            "snapshot-recovery-commit",
            recovery_id,
            None,
        );
        workspace_registry
            .mark_recovery_ready(recovery_id)
            .expect("recovery should become ready");
        session_store
            .seed_execution_chain_for_test(
                session_id.clone(),
                chain(
                    &session_id,
                    &mission_id,
                    &root_task_id,
                    branch.clone(),
                    Some(recovery_id.to_string()),
                ),
            )
            .expect("chain should persist");
        session_store
            .set_recovery_id(&session_id, None, Some(recovery_id.into()))
            .unwrap();
        let mut active_chain = session_store
            .active_execution_chain(&session_id)
            .expect("active chain should exist");

        let commit = apply_chain_recovery_if_needed(
            &session_store,
            &workspace_registry,
            None,
            &session_id,
            &mut active_chain,
            &branch,
        )
        .expect("recovery should apply")
        .expect("recovery commit should be required");
        assert_eq!(
            workspace_registry
                .recovery_sidecar_export(recovery_id)
                .expect("recovery should exist")
                .current_status,
            RecoveryStatus::Ready
        );
        assert_eq!(
            session_store.recovery_id(&session_id).as_deref(),
            Some(recovery_id)
        );

        session_store
            .set_recovery_id(&session_id, Some(recovery_id), Some("new-recovery".into()))
            .unwrap();
        assert!(
            commit_chain_recovery(
                &session_store,
                &workspace_registry,
                &session_id,
                commit.clone(),
            )
            .is_err()
        );
        assert_eq!(
            session_store.recovery_id(&session_id).as_deref(),
            Some("new-recovery")
        );
        assert_eq!(
            session_store.recovery_id(&session_id).as_deref(),
            Some("new-recovery")
        );
        assert_eq!(
            workspace_registry
                .recovery_sidecar_export(recovery_id)
                .unwrap()
                .current_status,
            RecoveryStatus::Ready
        );
        session_store
            .set_recovery_id(&session_id, Some("new-recovery"), Some(recovery_id.into()))
            .unwrap();

        commit_chain_recovery(&session_store, &workspace_registry, &session_id, commit)
            .expect("recovery commit should succeed");
        assert_eq!(
            workspace_registry
                .recovery_sidecar_export(recovery_id)
                .expect("recovery should exist")
                .current_status,
            RecoveryStatus::Consumed
        );
        assert!(session_store.recovery_id(&session_id).is_none());
    }

    #[test]
    fn failed_recovery_paths_are_idempotent_and_revoke_active_lease() {
        let task_store = TaskStore::new();
        let session_id = SessionId::new("session-failed-path");
        let mission_id = MissionId::new("mission-failed-path");
        let root_task_id = TaskId::new("task-root-failed-path");
        let branch_task_id = TaskId::new("task-branch-failed-path");
        task_store
            .insert_task(task(
                root_task_id.as_str(),
                root_task_id.as_str(),
                mission_id.as_str(),
                None,
                TaskStatus::Running,
            ))
            .expect("root should insert");
        task_store
            .insert_task(task(
                branch_task_id.as_str(),
                root_task_id.as_str(),
                mission_id.as_str(),
                Some(root_task_id.as_str()),
                TaskStatus::Pending,
            ))
            .expect("branch should insert");
        let worker_id = WorkerId::new("worker-failed-path");
        let lease = task_store
            .grant_lease_and_start_task(
                &branch_task_id,
                &root_task_id,
                &worker_id,
                "executor",
                60_000,
            )
            .expect("lease should grant")
            .expect("lease should be active");
        let chain = chain(
            &session_id,
            &mission_id,
            &root_task_id,
            branch(branch_task_id.as_str(), worker_id.as_str()),
            None,
        );

        fail_resumed_execution_paths(&task_store, &chain, &chain.branches)
            .expect("failure path should close");
        fail_resumed_execution_paths(&task_store, &chain, &chain.branches)
            .expect("repeated failure path should be idempotent");
        assert_eq!(
            task_store.get_task(&root_task_id).unwrap().status,
            TaskStatus::Failed
        );
        assert_eq!(
            task_store.get_task(&branch_task_id).unwrap().status,
            TaskStatus::Failed
        );
        assert_eq!(
            task_store.get_lease(&lease.lease_id).unwrap().lease_status,
            magi_orchestrator::task_store::TaskLeaseState::Revoked
        );
    }
}
