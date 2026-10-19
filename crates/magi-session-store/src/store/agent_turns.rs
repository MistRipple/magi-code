//! 独立代理 Turn 与主线使用同一 canonical writer，不改变 Session 当前输入指针。
use super::sidecar::{CanonicalCommitPlan, prepare_canonical_turn_in_state};
use super::{CanonicalTurnMutation, SessionStore};
use crate::agent_turn::AGENT_EXECUTION_METADATA_KEY;
use crate::{
    ActiveExecutionTurnItem, AgentTurnBinding, CanonicalTurn, CanonicalTurnStatus,
    ExecutionThreadStatus, SessionStoreState,
};
use magi_core::{
    DomainError, DomainResult, SessionId, SessionLifecycleStatus, Task, TaskId, TaskKind,
    TaskStatus, ThreadId, UtcMillis,
};
use serde_json::Value;

fn invalid(message: impl Into<String>) -> DomainError {
    DomainError::InvalidState {
        message: message.into(),
    }
}

fn require_active_session(state: &SessionStoreState, session: &SessionId) -> DomainResult<()> {
    if state.sessions.iter().any(|record| {
        &record.session_id == session && record.status == SessionLifecycleStatus::Active
    }) {
        Ok(())
    } else {
        Err(invalid("Agent Turn 所属会话不存在或已关闭"))
    }
}

fn turn_for_task<'a>(
    state: &'a SessionStoreState,
    session: &SessionId,
    task: &TaskId,
) -> Option<&'a CanonicalTurn> {
    state
        .canonical_turns
        .iter()
        .filter(|turn| {
            &turn.session_id == session
                && turn
                    .metadata
                    .get(AGENT_EXECUTION_METADATA_KEY)
                    .and_then(|value| value.get("taskId"))
                    .and_then(Value::as_str)
                    == Some(task.as_str())
        })
        .max_by_key(|turn| turn.turn_seq)
}

impl SessionStore {
    /// 幂等回执只读原事实，不因重试而重新开放执行。
    pub fn agent_continuation_for_operation(
        &self,
        session: &SessionId,
        task: &TaskId,
        operation: &str,
    ) -> DomainResult<Option<CanonicalTurn>> {
        for turn in self.canonical_turns_for_session(session) {
            if turn.agent_turn_binding()?.is_some_and(|binding| {
                &binding.task_id == task
                    && binding.continuation_operation_id.as_deref() == Some(operation)
            }) {
                return Ok(Some(turn));
            }
        }
        Ok(None)
    }

    /// 同一事务接纳整个转交范围的新 Turn；原 Task/Thread、父链和主线指针不变。
    /// TaskStore 尚未重开，崩溃后按既有中断收口，不自动重放接纳意图。
    pub fn prepare_transferred_agent_continuation(
        &self,
        session: &SessionId,
        tasks: &[Task],
        source_turn_id: &str,
        operation: &str,
    ) -> DomainResult<Vec<CanonicalTurn>> {
        if !magi_core::agent_operation_id_is_valid(operation) || tasks.is_empty() {
            return Err(invalid("代理继续缺少有效 operation 或任务来源"));
        }
        self.commit_canonical_transaction(
            session,
            |state| {
                require_active_session(state, session)?;
                let root = &tasks[0];
                let transfer = root
                    .agent_run_transfer()
                    .ok_or_else(|| invalid("只能独立继续已转交子树根"))?;
                transfer.validate(
                    root.agent_activation()
                        .ok_or_else(|| invalid("代理缺少持久身份"))?,
                )?;
                validate_transferred_owner(state, transfer)?;
                let original = turn_for_task(state, session, &root.task_id)
                    .ok_or_else(|| invalid("代理缺少原 Turn"))?;
                if original.turn_id != source_turn_id {
                    return Err(invalid("独立继续来源 Turn 已变化"));
                }
                let mut resumed = Vec::<CanonicalTurn>::new();
                let mut sequence = state
                    .canonical_turns
                    .iter()
                    .filter(|turn| &turn.session_id == session)
                    .map(|turn| turn.turn_seq)
                    .max()
                    .unwrap_or(0);
                for task in tasks {
                    let previous = turn_for_task(state, session, &task.task_id)
                        .ok_or_else(|| invalid("恢复任务缺少独立 Turn"))?;
                    let mut binding = previous
                        .agent_turn_binding()?
                        .ok_or_else(|| invalid("恢复目标不是 Agent Turn"))?;
                    if task.status != TaskStatus::Failed
                        || previous.status != CanonicalTurnStatus::Interrupted
                        || binding.root_task_id != task.root_task_id
                        || binding.mission_id != task.mission_id
                        || task.parent_task_id.as_ref() != Some(&binding.parent_task_id)
                        || task
                            .agent_activation()
                            .is_none_or(|identity| &identity.session_id != session)
                        || resolve_agent_run_boundary(self, state, previous)?
                            .is_none_or(|(_, owner)| &owner != transfer)
                    {
                        return Err(invalid("恢复任务与原 Turn 或转交范围不匹配"));
                    }
                    for item in &previous.items {
                        if item.user_question()?.is_some_and(|question| {
                            question.response.is_none() && question.cancelled_at.is_none()
                        }) || item
                            .tool_approval()?
                            .is_some_and(|approval| approval.is_pending(UtcMillis::now()))
                        {
                            return Err(invalid("请先处理原代理的问题或审批，再显式继续"));
                        }
                    }
                    binding.previous_turn_id = Some(previous.turn_id.clone());
                    binding.continuation_operation_id = Some(operation.into());
                    validate_agent_recovery_source(state, previous, &binding)?;
                    if task.task_id != root.task_id {
                        let parent = resumed
                            .iter()
                            .find(|turn| {
                                turn.agent_turn_binding()
                                    .ok()
                                    .flatten()
                                    .is_some_and(|parent| parent.task_id == binding.parent_task_id)
                            })
                            .ok_or_else(|| invalid("代理继续必须先接纳直接父轮"))?;
                        if parent
                            .agent_turn_binding()?
                            .and_then(|parent| parent.previous_turn_id)
                            .as_deref()
                            != Some(binding.parent_turn_id.as_str())
                        {
                            return Err(invalid("嵌套继续没有绑定原直接父轮"));
                        }
                        binding.parent_turn_id = parent.turn_id.clone();
                    }
                    sequence = sequence
                        .checked_add(1)
                        .ok_or_else(|| invalid("Turn sequence 溢出"))?;
                    let mut turn = previous.clone();
                    turn.turn_id = binding.turn_id();
                    turn.turn_seq = sequence;
                    turn.accepted_at = UtcMillis(
                        UtcMillis::now()
                            .0
                            .max(previous.completed_at.unwrap_or(previous.accepted_at).0),
                    );
                    turn.status = CanonicalTurnStatus::Running;
                    turn.completed_at = None;
                    turn.response_duration_ms = None;
                    turn.usage = None;
                    turn.items.clear();
                    turn.model_steps.clear();
                    turn.metadata.insert(
                        AGENT_EXECUTION_METADATA_KEY.into(),
                        serde_json::to_value(&binding).expect("agent binding serializes"),
                    );
                    turn.validate_model_steps()?;
                    if state
                        .canonical_turns
                        .iter()
                        .any(|old| old.session_id == *session && old.turn_id == turn.turn_id)
                    {
                        return Err(invalid("代理继续 operation 已使用"));
                    }
                    resumed.push(turn);
                }
                Ok(CanonicalCommitPlan {
                    mutations: resumed
                        .iter()
                        .map(|turn| CanonicalTurnMutation {
                            previous: None,
                            next: turn.clone(),
                        })
                        .collect(),
                    acceptance: None,
                    value: resumed,
                })
            },
            |_, value| value,
        )
    }

    /// 转交只写 TaskStore，但准入必须与 canonical Turn/Goal 控制互斥。
    /// callback 只能提交 TaskStore，不得回调 SessionStore 或自行修改 canonical。
    /// 在同一会话提交锁下验证后释放 state 读锁，避免 checkpoint 持锁访问磁盘时
    /// 阻塞普通读取；持久写入失败不会产生第二份拥有方事实。
    pub fn commit_agent_run_transfer<T>(
        &self,
        task: &Task,
        transfer: &magi_core::AgentRunTransfer,
        commit: impl FnOnce() -> DomainResult<T>,
    ) -> DomainResult<T> {
        let _barrier = self
            .canonical_commit_barrier
            .read()
            .expect("canonical barrier poisoned");
        let lock = self.canonical_commit_lock_for(&transfer.session_id);
        let _guard = lock.lock().expect("canonical session lock poisoned");
        {
            let state = self.state.read().expect("session state read lock poisoned");
            require_active_session(&state, &transfer.session_id)?;
            let activation = task
                .agent_activation()
                .ok_or_else(|| invalid("转交任务缺少 Agent 身份"))?;
            transfer.validate(activation)?;
            let turn = turn_for_task(&state, &transfer.session_id, &task.task_id)
                .ok_or_else(|| invalid("转交任务缺少独立 Agent Turn"))?;
            let binding = turn
                .agent_turn_binding()?
                .ok_or_else(|| invalid("转交缺少 canonical 绑定"))?;
            if turn.status.is_terminal()
                || turn.completed_at.is_some()
                || binding.parent_task_id != transfer.transferred_by
                || binding.parent_turn_id != transfer.source_turn_id
                || binding.mission_id != task.mission_id
                || binding.root_task_id != task.root_task_id
            {
                return Err(invalid(
                    "转交与 Agent 的原始 Task/Turn 来源不匹配或执行已结束",
                ));
            }
            validate_agent_turn_admission(self, &state, turn)?;
            let source = state
                .canonical_turns
                .iter()
                .find(|source| {
                    source.session_id == transfer.session_id
                        && source.turn_id == transfer.source_turn_id
                })
                .ok_or_else(|| invalid("转交来源 Turn 缺失"))?;
            if source.status.is_terminal()
                || source.completed_at.is_some()
                || transfer.transferred_at < source.accepted_at
            {
                return Err(invalid("来源 Turn 已结束或转交时间无效"));
            }
            validate_transferred_owner(&state, transfer)?;
        }
        commit()
    }

    /// 只安装查询端口；TaskStore 仍拥有转交事实。恢复和最小装配未接线时不授予独立运行权。
    pub fn set_agent_run_transfer_resolver(
        &self,
        resolver: std::sync::Arc<crate::AgentRunTransferResolver>,
    ) {
        *self
            .agent_run_transfer_resolver
            .write()
            .expect("agent run resolver lock poisoned") = Some(resolver);
    }

    /// 执行、资源准备和等待恢复共用的 canonical 准入投影。只读现有 Session、
    /// Turn、Goal 与父链，不持有另一份执行拥有方或运行状态。
    pub fn turn_execution_is_active(&self, session: &SessionId, turn_id: &str) -> bool {
        let state = self.state.read().expect("session state lock poisoned");
        let Some(turn) = state
            .canonical_turns
            .iter()
            .find(|turn| &turn.session_id == session && turn.turn_id == turn_id)
        else {
            return false;
        };
        require_active_session(&state, session).is_ok()
            && !turn.status.is_terminal()
            && turn.completed_at.is_none()
            && validate_agent_turn_admission(self, &state, turn).is_ok()
            && execution_goal_in_state(self, &state, turn)
                .is_ok_and(|goal| goal.is_none_or(|goal| goal.status != crate::GoalStatus::Paused))
    }

    /// Task 结果结算读取原 Turn 的取消事实。子代理沿不可变父链定位取消，
    /// 不依赖主会话当前指针，也不改写旧 Turn 或丢失迟到响应的用量。
    pub fn turn_execution_was_cancelled(&self, session: &SessionId, turn_id: &str) -> bool {
        let state = self.state.read().expect("session state lock poisoned");
        let boundary = state
            .canonical_turns
            .iter()
            .find(|turn| &turn.session_id == session && turn.turn_id == turn_id)
            .and_then(|turn| {
                resolve_agent_run_boundary(self, &state, turn)
                    .ok()
                    .flatten()
            });
        let mut id = turn_id.to_owned();
        let mut seen = std::collections::HashSet::new();
        loop {
            if !seen.insert(id.clone()) {
                return false;
            }
            let Some(turn) = state
                .canonical_turns
                .iter()
                .find(|turn| &turn.session_id == session && turn.turn_id == id)
            else {
                return false;
            };
            if turn.status == CanonicalTurnStatus::Cancelled {
                return true;
            }
            if let Some((boundary_turn, transfer)) = &boundary
                && &turn.turn_id == boundary_turn
            {
                // 原父轮的取消不再属于这个执行；新拥有方的撤销仍终止本轮。
                return validate_transferred_owner(&state, transfer).is_err();
            }
            match turn.agent_turn_binding() {
                Ok(Some(binding)) => id = binding.parent_turn_id,
                _ => return false,
            }
        }
    }

    /// 从 canonical 事实派生发布用视图，不写入 Session 当前指针。
    pub fn project_canonical_execution_turn(
        &self,
        turn: &CanonicalTurn,
    ) -> crate::ActiveExecutionTurn {
        super::sidecar::active_turn_projection(turn)
    }

    /// Task 与 Thread 已在唯一注册入口完成校验后调用。首次写入失败不会发布任何执行权。
    pub fn create_agent_turn(
        &self,
        task: &Task,
        parent_turn_id: &str,
        thread_id: &ThreadId,
    ) -> DomainResult<CanonicalTurn> {
        let activation = task
            .agent_activation()
            .ok_or_else(|| invalid("子代理缺少 activation 身份"))?;
        activation.validate(&task.task_id)?;
        let session = &activation.session_id;
        let parent_task_id = task
            .parent_task_id
            .clone()
            .ok_or_else(|| invalid("主线任务不能创建 Agent Turn"))?;
        let binding_parent_task = parent_task_id;
        let binding_parent_turn = parent_turn_id.to_owned();
        self.commit_canonical_transaction(
            session,
            |state| {
                require_active_session(state, session)?;
                let thread = state
                    .thread_registry
                    .iter()
                    .find(|thread| {
                        &thread.session_id == session
                            && &thread.thread_id == thread_id
                            && thread.mission_id == task.mission_id
                            && thread.status != ExecutionThreadStatus::Retired
                            && thread.handled_task_ids.as_slice() == [task.task_id.clone()]
                    })
                    .ok_or_else(|| invalid("Agent Turn 需要本 Task 独占的 Thread"))?;
                let mut binding = AgentTurnBinding {
                    task_id: task.task_id.clone(),
                    root_task_id: task.root_task_id.clone(),
                    parent_task_id: binding_parent_task.clone(),
                    parent_turn_id: binding_parent_turn.clone(),
                    thread_id: thread_id.clone(),
                    mission_id: thread.mission_id.clone(),
                    worker_id: thread.worker_instance_id.clone(),
                    role_id: thread.role_id.clone(),
                    previous_turn_id: None,
                    continuation_operation_id: None,
                };
                if task.kind != TaskKind::LocalAgent
                    || !matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                {
                    return Err(invalid("只有未结束的 LocalAgent 子任务可以接纳独立 Turn"));
                }
                if let Some(existing) = turn_for_task(state, session, &task.task_id) {
                    let old = existing
                        .agent_turn_binding()?
                        .ok_or_else(|| invalid("Agent Turn 缺少来源"))?;
                    if old.parent_turn_id == parent_turn_id {
                        binding.previous_turn_id = old.previous_turn_id.clone();
                        if old != binding {
                            return Err(invalid("同一 Agent Task 已绑定其他执行来源"));
                        }
                        return Ok(CanonicalCommitPlan {
                            mutations: vec![],
                            value: existing.clone(),
                            acceptance: None,
                        });
                    }
                    binding.previous_turn_id = Some(existing.turn_id.clone());
                    validate_agent_recovery_source(state, existing, &binding)?;
                }
                let turn_id = binding.turn_id();
                if state
                    .canonical_turns
                    .iter()
                    .any(|turn| &turn.session_id == session && turn.turn_id == turn_id)
                {
                    return Err(invalid("Agent Turn 身份与已有 canonical 记录冲突"));
                }
                let parent = state
                    .canonical_turns
                    .iter()
                    .find(|turn| &turn.session_id == session && turn.turn_id == parent_turn_id)
                    .ok_or_else(|| invalid("Agent Turn 的来源父 Turn 不存在或已关闭"))?;
                if parent.status.is_terminal() || parent.completed_at.is_some() {
                    return Err(invalid("Agent Turn 的来源父 Turn 已关闭，未授予独立运行权"));
                }
                let parent_matches = if let Some(parent_binding) = parent.agent_turn_binding()? {
                    parent_binding.task_id == binding.parent_task_id
                        && parent_binding.root_task_id == binding.root_task_id
                } else {
                    binding.parent_task_id == binding.root_task_id
                        && (parent
                            .model_steps
                            .iter()
                            .any(|step| step.task_id.as_ref() == Some(&binding.parent_task_id))
                            || parent.items.iter().any(|item| {
                                item.worker
                                    .as_ref()
                                    .and_then(|worker| worker.task_id.as_ref())
                                    == Some(&binding.parent_task_id)
                            }))
                };
                if !parent_matches {
                    return Err(invalid("Agent Turn 的父任务不属于来源执行"));
                }
                let accepted_at = UtcMillis(
                    UtcMillis::now()
                        .0
                        .max(parent.accepted_at.0)
                        .max(thread.created_at.0),
                );
                let turn_seq = state
                    .canonical_turns
                    .iter()
                    .filter(|turn| &turn.session_id == session)
                    .map(|turn| turn.turn_seq)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or_else(|| invalid("Turn sequence 溢出"))?;
                let mut metadata = std::collections::HashMap::from([(
                    AGENT_EXECUTION_METADATA_KEY.into(),
                    serde_json::to_value(&binding).map_err(|error| invalid(error.to_string()))?,
                )]);
                let prior_skill =
                    turn_for_task(state, session, &task.task_id).and_then(|previous| {
                        previous
                            .metadata
                            .get(crate::agent_turn::AGENT_ACTIVE_SKILL_METADATA_KEY)
                    });
                let skill = if let Some(prior) = prior_skill {
                    prior.clone()
                } else {
                    let branch_skill = state
                        .execution_sidecar_store
                        .runtime_sidecars
                        .iter()
                        .find(|sidecar| &sidecar.session_id == session)
                        .and_then(|sidecar| sidecar.active_execution_chain.as_ref())
                        .filter(|chain| {
                            chain.root_task_id == task.root_task_id
                                && chain.mission_id == task.mission_id
                        })
                        .and_then(|chain| {
                            chain
                                .branches
                                .iter()
                                .find(|branch| branch.task_id == task.task_id)
                        })
                        .and_then(|branch| branch.skill_name.as_deref());
                    serde_json::to_value(
                        branch_skill.or_else(|| task.executor_binding_active_skill_id()),
                    )
                    .expect("Skill ID serializable")
                };
                metadata.insert(
                    crate::agent_turn::AGENT_ACTIVE_SKILL_METADATA_KEY.into(),
                    skill,
                );
                if let Some(goal) = state.goals.iter().find(|goal| {
                    &goal.session_id == session
                        && super::sidecar::goal_owns_turn(state, goal, parent)
                }) {
                    metadata.insert("goalId".into(), Value::String(goal.goal_id.to_string()));
                }
                let turn = CanonicalTurn {
                    session_id: session.clone(),
                    turn_id: turn_id.clone(),
                    turn_seq,
                    accepted_at,
                    completed_at: None,
                    status: CanonicalTurnStatus::Running,
                    response_duration_ms: None,
                    usage: None,
                    items: vec![],
                    model_steps: vec![],
                    metadata,
                };
                turn.validate_model_steps()?;
                Ok(CanonicalCommitPlan {
                    mutations: vec![CanonicalTurnMutation {
                        previous: None,
                        next: turn.clone(),
                    }],
                    value: turn,
                    acceptance: None,
                })
            },
            |_, turn| turn,
        )
    }

    pub fn agent_turn_for_task(
        &self,
        session: &SessionId,
        task: &TaskId,
    ) -> DomainResult<Option<CanonicalTurn>> {
        let state = self.state.read().expect("session state lock poisoned");
        let Some(turn) = turn_for_task(&state, session, task) else {
            return Ok(None);
        };
        turn.agent_turn_binding()?
            .ok_or_else(|| invalid("Agent Turn ID 与普通 Turn 冲突"))?;
        Ok(Some(turn.clone()))
    }

    /// Skill 激活只更新自己的 canonical Turn；不依赖主线当前指针，也不能修改兄弟或旧轮。
    pub fn update_agent_turn_active_skill(
        &self,
        session: &SessionId,
        task: &TaskId,
        turn_id: &str,
        skill_id: &str,
    ) -> DomainResult<()> {
        if skill_id.trim().is_empty() {
            return Err(invalid("Skill ID 不能为空"));
        }
        self.commit_canonical_transaction(
            session,
            |state| {
                let previous = turn_for_task(state, session, task)
                    .filter(|turn| turn.turn_id == turn_id)
                    .ok_or_else(|| invalid("Skill 激活目标 Agent Turn 已变化"))?;
                validate_agent_turn_admission(self, state, previous)?;
                let mut next = previous.clone();
                next.metadata.insert(
                    crate::agent_turn::AGENT_ACTIVE_SKILL_METADATA_KEY.into(),
                    Value::String(skill_id.into()),
                );
                let mutations = if next == *previous {
                    vec![]
                } else {
                    vec![CanonicalTurnMutation {
                        previous: Some(previous.clone()),
                        next,
                    }]
                };
                Ok(CanonicalCommitPlan {
                    mutations,
                    value: (),
                    acceptance: None,
                })
            },
            |_, ()| (),
        )
    }

    /// daemon 已经结算恢复的 Task/lease 后，补齐所有独立 Agent Turn 的终态。
    /// 不依赖主线 current_turn/active_execution_chain；不创建恢复执行或新 Step。
    pub fn reconcile_agent_turns_after_restart(
        &self,
        session: &SessionId,
        tasks: &[Task],
    ) -> DomainResult<usize> {
        self.commit_canonical_transaction(
            session,
            |state| {
                let mut mutations = Vec::new();
                for previous in state
                    .canonical_turns
                    .iter()
                    .filter(|turn| &turn.session_id == session && !turn.status.is_terminal())
                {
                    let Some(binding) = previous.agent_turn_binding()? else {
                        continue;
                    };
                    let task = tasks
                        .iter()
                        .find(|task| task.task_id == binding.task_id)
                        .ok_or_else(|| invalid("重启 Agent Turn 缺少原 Task，不能猜测执行结果"))?;
                    if task.kind != TaskKind::LocalAgent
                        || task.root_task_id != binding.root_task_id
                        || task.parent_task_id.as_ref() != Some(&binding.parent_task_id)
                        || task.mission_id != binding.mission_id
                        || task
                            .agent_activation()
                            .is_none_or(|activation| &activation.session_id != session)
                    {
                        return Err(invalid("重启 Agent Turn 与 Task 的来源不匹配"));
                    }
                    let status = match task.status {
                        TaskStatus::Completed => CanonicalTurnStatus::Completed,
                        TaskStatus::Killed => CanonicalTurnStatus::Cancelled,
                        TaskStatus::Failed => CanonicalTurnStatus::Interrupted,
                        TaskStatus::Pending | TaskStatus::Running => {
                            return Err(invalid(
                                "重启 Agent Task/lease 尚未结算，不能发布 Turn 中断",
                            ));
                        }
                    };
                    mutations.push(CanonicalTurnMutation {
                        previous: Some(previous.clone()),
                        next: prepare_terminal_agent_turn(state, previous, status)?,
                    });
                }
                Ok(CanonicalCommitPlan {
                    value: mutations.len(),
                    mutations,
                    acceptance: None,
                })
            },
            |_, count| count,
        )
    }

    /// 终态只能由 Task 生命周期结算方提交；迟到模型响应仍可仅结算原 Step 用量。
    pub fn finish_agent_turn(
        &self,
        session: &SessionId,
        task: &TaskId,
        turn_id: &str,
        status: CanonicalTurnStatus,
    ) -> DomainResult<Option<(CanonicalTurn, bool)>> {
        if !matches!(
            status,
            CanonicalTurnStatus::Completed
                | CanonicalTurnStatus::Failed
                | CanonicalTurnStatus::Cancelled
                | CanonicalTurnStatus::Interrupted
        ) {
            return Err(invalid("Agent Turn 只能提交明确终态"));
        }
        self.commit_canonical_transaction(
            session,
            |state| {
                let Some(previous) = state
                    .canonical_turns
                    .iter()
                    .find(|turn| &turn.session_id == session && turn.turn_id == turn_id)
                else {
                    return Ok(CanonicalCommitPlan {
                        mutations: vec![],
                        value: None,
                        acceptance: None,
                    });
                };
                let binding = previous
                    .agent_turn_binding()?
                    .ok_or_else(|| invalid("目标不是 Agent Turn"))?;
                if &binding.task_id != task {
                    return Err(invalid("Agent 终态 Task 与 Turn 不匹配"));
                }
                if previous.status.is_terminal() {
                    if previous.status != status {
                        return Err(invalid("Agent Turn 已以其他状态结算"));
                    }
                    return Ok(CanonicalCommitPlan {
                        mutations: vec![],
                        value: Some((previous.clone(), false)),
                        acceptance: None,
                    });
                }
                let next = prepare_terminal_agent_turn(state, previous, status)?;
                Ok(CanonicalCommitPlan {
                    mutations: vec![CanonicalTurnMutation {
                        previous: Some(previous.clone()),
                        next: next.clone(),
                    }],
                    value: Some((next, true)),
                    acceptance: None,
                })
            },
            |_, result| result,
        )
    }

    /// 由可信 Task 写回入口指定 Task/Turn，不允许通过 Session 当前指针猜测目标。
    pub fn upsert_agent_turn_item(
        &self,
        session: &SessionId,
        task: &TaskId,
        turn_id: &str,
        item: ActiveExecutionTurnItem,
    ) -> DomainResult<CanonicalTurn> {
        self.commit_canonical_transaction(
            session,
            |state| {
                require_active_session(state, session)?;
                let previous = turn_for_task(state, session, task)
                    .filter(|turn| turn.turn_id == turn_id)
                    .ok_or_else(|| invalid("Agent 写回目标或 Task 不匹配"))?;
                let binding = previous
                    .agent_turn_binding()?
                    .ok_or_else(|| invalid("目标不是 Agent Turn"))?;
                validate_agent_turn_admission(self, state, previous)?;
                if previous.status.is_terminal()
                    || previous.completed_at.is_some()
                    || item.task_id.as_ref() != Some(task)
                    || item.source_thread_id != binding.thread_id
                {
                    return Err(invalid("Agent Turn 已关闭或 item 来源不匹配"));
                }
                let mut projected = super::sidecar::active_turn_projection(previous);
                super::sidecar::upsert_item_in_turn(&mut projected, item.clone())?;
                let next = prepare_canonical_turn_in_state(state, session, &projected)?;
                Ok(CanonicalCommitPlan {
                    mutations: if &next == previous {
                        vec![]
                    } else {
                        vec![CanonicalTurnMutation {
                            previous: Some(previous.clone()),
                            next: next.clone(),
                        }]
                    },
                    value: next,
                    acceptance: None,
                })
            },
            |_, turn| turn,
        )
    }
}

fn validate_agent_recovery_source(
    state: &SessionStoreState,
    previous: &CanonicalTurn,
    binding: &AgentTurnBinding,
) -> DomainResult<()> {
    let old = previous
        .agent_turn_binding()?
        .ok_or_else(|| invalid("Agent 恢复来源缺少绑定"))?;
    if previous.status != CanonicalTurnStatus::Interrupted
        || binding.task_id != old.task_id
        || binding.root_task_id != old.root_task_id
        || binding.parent_task_id != old.parent_task_id
        || binding.thread_id != old.thread_id
        || binding.mission_id != old.mission_id
        || binding.worker_id != old.worker_id
        || binding.role_id != old.role_id
        || binding.previous_turn_id.as_deref() != Some(previous.turn_id.as_str())
    {
        return Err(invalid("只能恢复同一 Task/Thread 的中断 Agent Turn"));
    }
    if let Some(operation) = binding.continuation_operation_id.as_deref() {
        if operation.trim().is_empty() {
            return Err(invalid("独立继续缺少有效 operation"));
        }
        if binding.parent_turn_id == old.parent_turn_id {
            return Ok(());
        }
    }
    let mut expected = binding.clone();
    expected.parent_turn_id = old.parent_turn_id.clone();
    expected.previous_turn_id = old.previous_turn_id.clone();
    expected.continuation_operation_id = old.continuation_operation_id.clone();
    if old != expected {
        return Err(invalid("只能恢复同一 Task/Thread 的中断 Agent Turn"));
    }
    let parent = state
        .canonical_turns
        .iter()
        .find(|turn| {
            turn.session_id == previous.session_id && turn.turn_id == binding.parent_turn_id
        })
        .ok_or_else(|| invalid("Agent 恢复父 Turn 缺失"))?;
    let source = if let Some(parent_binding) = parent.agent_turn_binding()? {
        parent_binding.previous_turn_id
    } else {
        parent.items.iter().find_map(|item| {
            item.metadata
                .get("recoverySourceTurnId")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
    };
    if source.as_deref() != Some(old.parent_turn_id.as_str()) {
        return Err(invalid("Agent 恢复未绑定原父执行的显式 Continue"));
    }
    Ok(())
}

pub(super) fn restore_agent_turn_threads(state: &mut SessionStoreState) -> DomainResult<()> {
    for turn in &state.canonical_turns {
        let Some(binding) = turn.agent_turn_binding()? else {
            continue;
        };
        if state
            .thread_registry
            .iter()
            .any(|thread| thread.thread_id == binding.thread_id)
        {
            continue;
        }
        state.thread_registry.push(crate::ExecutionThread {
            thread_id: binding.thread_id,
            session_id: turn.session_id.clone(),
            mission_id: binding.mission_id,
            role_id: binding.role_id,
            worker_instance_id: binding.worker_id,
            status: if turn.status.is_terminal() {
                ExecutionThreadStatus::Idle
            } else {
                ExecutionThreadStatus::Active
            },
            created_at: turn.accepted_at,
            last_used_at: turn.completed_at.unwrap_or(turn.accepted_at),
            observed_context_window_tokens: None,
            handled_task_ids: vec![binding.task_id],
            message_history: vec![],
        });
    }
    Ok(())
}

/// 恢复只读验证完整来源链；关闭的父轮可以保留审计，不能因缺失来源猜测授权。
pub(super) fn validate_agent_turn_graph(state: &SessionStoreState) -> DomainResult<()> {
    for turn in &state.canonical_turns {
        let Some(binding) = turn.agent_turn_binding()? else {
            continue;
        };
        if !state
            .sessions
            .iter()
            .any(|record| record.session_id == turn.session_id)
        {
            return Err(invalid("Agent Turn 引用不存在的会话"));
        }
        let thread_matches = state.thread_registry.iter().any(|thread| {
            thread.session_id == turn.session_id
                && thread.thread_id == binding.thread_id
                && thread.mission_id == binding.mission_id
                && thread.worker_instance_id == binding.worker_id
                && thread.role_id == binding.role_id
                && thread.handled_task_ids.as_slice() == [binding.task_id.clone()]
        });
        if !thread_matches {
            return Err(invalid("Agent Turn 与持久 Thread 归属不匹配"));
        }
        if let Some(previous_id) = &binding.previous_turn_id {
            let previous = state
                .canonical_turns
                .iter()
                .find(|previous| {
                    previous.session_id == turn.session_id
                        && &previous.turn_id == previous_id
                        && previous.turn_seq < turn.turn_seq
                })
                .ok_or_else(|| invalid("Agent 恢复来源缺失或顺序错误"))?;
            validate_agent_recovery_source(state, previous, &binding)?;
            if state.canonical_turns.iter().any(|other| {
                other.session_id == turn.session_id
                    && other.turn_id != turn.turn_id
                    && other
                        .metadata
                        .get(AGENT_EXECUTION_METADATA_KEY)
                        .and_then(|value| value.get("previousTurnId"))
                        .and_then(Value::as_str)
                        == Some(previous_id.as_str())
            }) {
                return Err(invalid("同一 Agent Turn 不能派生两个恢复执行"));
            }
        }
        let mut current = turn;
        let mut seen = std::collections::HashSet::new();
        while let Some(owner) = current.agent_turn_binding()? {
            if !seen.insert(current.turn_id.as_str()) {
                return Err(invalid("Agent Turn 来源链循环"));
            }
            let parent = state
                .canonical_turns
                .iter()
                .find(|candidate| {
                    candidate.session_id == turn.session_id
                        && candidate.turn_id == owner.parent_turn_id
                })
                .ok_or_else(|| invalid("Agent Turn 父轮缺失或跨会话"))?;
            if let Some(parent_owner) = parent.agent_turn_binding()? {
                if parent_owner.task_id != owner.parent_task_id
                    || parent_owner.root_task_id != owner.root_task_id
                    || parent_owner.mission_id != owner.mission_id
                {
                    return Err(invalid("Agent Turn 父任务或根归属变化"));
                }
            } else if owner.parent_task_id != owner.root_task_id
                || !(parent
                    .model_steps
                    .iter()
                    .any(|step| step.task_id.as_ref() == Some(&owner.parent_task_id))
                    || parent.items.iter().any(|item| {
                        item.worker
                            .as_ref()
                            .and_then(|worker| worker.task_id.as_ref())
                            == Some(&owner.parent_task_id)
                    }))
            {
                return Err(invalid("Agent Turn 根来源没有父任务执行证据"));
            }
            current = parent;
        }
    }
    Ok(())
}

/// 核对查询返回的转交与 canonical 来源链，保留原父任务/Turn 用于审计。
/// 只有命中该 TaskStore 子树根的同一 Agent Turn 才能建立运行边界。
fn resolve_agent_run_boundary(
    store: &SessionStore,
    state: &SessionStoreState,
    turn: &CanonicalTurn,
) -> DomainResult<Option<(String, magi_core::AgentRunTransfer)>> {
    let Some(origin) = turn.agent_turn_binding()? else {
        return Ok(None);
    };
    let resolver = store
        .agent_run_transfer_resolver
        .read()
        .expect("agent run resolver lock poisoned")
        .clone();
    let Some(resolve) = resolver else {
        // 无 TaskStore 的最小装配只能使用原父轮准入，不能猜测独立执行权。
        return Ok(None);
    };
    let Some(root) = resolve(&origin.task_id)? else {
        return Ok(None);
    };
    let transfer = root
        .agent_run_transfer()
        .ok_or_else(|| invalid("转交查询缺少 TaskStore 原记录"))?;
    let activation = root
        .agent_activation()
        .ok_or_else(|| invalid("转交根缺少 Agent activation"))?;
    activation.validate(&root.task_id)?;
    transfer.validate(activation)?;
    if root.kind != TaskKind::LocalAgent
        || root.root_task_id != origin.root_task_id
        || root.mission_id != origin.mission_id
        || transfer.session_id != turn.session_id
    {
        return Err(invalid("转交来源会话或 activation 身份无效"));
    }
    let mut current = turn;
    let mut seen = std::collections::HashSet::new();
    while let Some(binding) = current.agent_turn_binding()? {
        if !seen.insert(current.turn_id.as_str())
            || binding.root_task_id != origin.root_task_id
            || binding.mission_id != origin.mission_id
        {
            return Err(invalid("转交 canonical 来源链循环或归属变化"));
        }
        let parent = state
            .canonical_turns
            .iter()
            .find(|parent| {
                parent.session_id == turn.session_id && parent.turn_id == binding.parent_turn_id
            })
            .ok_or_else(|| invalid("转交 canonical 来源父轮缺失"))?;
        if binding.task_id == root.task_id {
            if binding.parent_task_id != transfer.transferred_by
                || root.parent_task_id.as_ref() != Some(&binding.parent_task_id)
                || binding.parent_turn_id != transfer.source_turn_id
                || transfer.transferred_at < root.created_at
                || transfer.transferred_at < parent.accepted_at
                || parent
                    .completed_at
                    .is_some_and(|at| at < transfer.transferred_at)
            {
                return Err(invalid("转交记录与原父任务、Turn 或接纳时间不匹配"));
            }
            return Ok(Some((current.turn_id.clone(), transfer.clone())));
        }
        current = parent;
    }
    Err(invalid("转交子树根不属于 canonical Agent 父链"))
}

fn validate_transferred_owner(
    state: &SessionStoreState,
    transfer: &magi_core::AgentRunTransfer,
) -> DomainResult<()> {
    require_active_session(state, &transfer.session_id)?;
    if let magi_core::AgentRunOwner::Goal {
        goal_id,
        control_revision,
    } = &transfer.owner
        && !state.goals.iter().any(|goal| {
            goal.session_id == transfer.session_id
                && &goal.goal_id == goal_id
                && goal.status == crate::GoalStatus::Active
                && goal.control_revision == *control_revision
        })
    {
        return Err(invalid("转交的 Goal 不存在、已暂停/结束或控制版本已撤销"));
    }
    Ok(())
}

/// 请求开始时按当前运行拥有方绑定 Goal；在途 Step 的既有用量归属不改写。
pub(super) fn execution_goal_in_state<'a>(
    store: &SessionStore,
    state: &'a SessionStoreState,
    turn: &CanonicalTurn,
) -> DomainResult<Option<&'a crate::SessionGoal>> {
    let boundary = resolve_agent_run_boundary(store, state, turn)?;
    if let Some((_, transfer)) = &boundary {
        validate_transferred_owner(state, transfer)?;
    }
    let owner = boundary.map(|(_, transfer)| transfer.owner);
    Ok(match owner {
        Some(magi_core::AgentRunOwner::Session) => None,
        Some(magi_core::AgentRunOwner::Goal { goal_id, .. }) => state
            .goals
            .iter()
            .find(|goal| goal.session_id == turn.session_id && goal.goal_id == goal_id),
        None => state.goals.iter().find(|goal| {
            goal.session_id == turn.session_id && super::sidecar::goal_owns_turn(state, goal, turn)
        }),
    })
}

/// 未转交仍检查完整父链；已转交检查当前子树与真实拥有方，不依赖主会话输入指针。
pub(super) fn validate_agent_turn_admission(
    store: &SessionStore,
    state: &SessionStoreState,
    turn: &CanonicalTurn,
) -> DomainResult<()> {
    if turn.agent_turn_binding()?.is_none() {
        return Ok(());
    }
    require_active_session(state, &turn.session_id)?;
    let boundary = resolve_agent_run_boundary(store, state, turn)?;
    let mut current = turn;
    let mut seen = std::collections::HashSet::new();
    while let Some(binding) = current.agent_turn_binding()? {
        if !seen.insert(current.turn_id.as_str()) {
            return Err(invalid("Agent Turn 来源链循环"));
        }
        if current.status.is_terminal() || current.completed_at.is_some() {
            return Err(invalid("Agent 子树中的执行 Turn 已关闭"));
        }
        if let Some((boundary_turn, transfer)) = &boundary
            && &current.turn_id == boundary_turn
        {
            return validate_transferred_owner(state, transfer);
        }
        let parent = state
            .canonical_turns
            .iter()
            .find(|parent| {
                parent.session_id == turn.session_id && parent.turn_id == binding.parent_turn_id
            })
            .ok_or_else(|| invalid("Agent 父 Turn 缺失"))?;
        if parent.status.is_terminal() || parent.completed_at.is_some() {
            return Err(invalid("Agent 所属父 Turn 已关闭，未授予独立运行权"));
        }
        current = parent;
    }
    Ok(())
}

/// 创建终态事实供既有 canonical 事务使用；不会单独写盘或切换主会话指针。
fn prepare_terminal_agent_turn(
    state: &SessionStoreState,
    previous: &CanonicalTurn,
    status: CanonicalTurnStatus,
) -> DomainResult<CanonicalTurn> {
    if status == CanonicalTurnStatus::Completed
        && (previous.items.iter().any(|item| {
            item.kind == crate::CanonicalTurnItemKind::ToolCall && !item.status.is_terminal()
        }) || previous
            .model_steps
            .iter()
            .any(|step| step.status == crate::ModelStepStatus::Started))
    {
        return Err(invalid("Agent 仍有未结算的模型请求或工具，不能报告完成"));
    }
    let mut projected = super::sidecar::active_turn_projection(previous);
    projected.status = super::sidecar::canonical_turn_status_name(status).into();
    let item_status = super::sidecar::terminal_item_status_for_turn_status(&projected.status)
        .expect("accepted terminal status");
    super::sidecar::settle_active_current_turn_items(&mut projected.items, item_status);
    if status != CanonicalTurnStatus::Completed {
        for (old, item) in previous.items.iter().zip(&mut projected.items) {
            if old.kind != crate::CanonicalTurnItemKind::ToolCall || old.status.is_terminal() {
                continue;
            }
            let waiting = old
                .tool
                .as_ref()
                .and_then(|tool| tool.result.as_ref())
                .is_some_and(|result| {
                    matches!(
                        result.get("status").and_then(Value::as_str),
                        Some("awaiting_approval" | "awaiting_user_input")
                    ) || result.get("execution_started").and_then(Value::as_bool) == Some(false)
                });
            if !waiting {
                item.status = "indeterminate".into();
                item.tool_status = Some("indeterminate".into());
                item.tool_result = Some(serde_json::json!({"status":"outcome_unknown","execution":"unknown",
                    "reason":"agent_execution_closed_before_tool_result","message":"执行已中断，工具结果尚未确认；请先核对状态，不要自动重复副作用。"}).to_string());
            }
        }
    }
    projected.completed_at = Some(UtcMillis(UtcMillis::now().0.max(previous.accepted_at.0)));
    let next = prepare_canonical_turn_in_state(state, &previous.session_id, &projected)?;
    Ok(next)
}

/// 主 Turn 的重启中断和子 Agent 的结算进入同一个 canonical event segment。
/// 不以进程内 Task 注册表决定恢复范围，避免 journal 已落盘而投影尚未保存时漏掉子轮。
pub(super) fn interrupted_descendant_turns(
    store: &SessionStore,
    state: &SessionStoreState,
    session: &SessionId,
    source_turn_id: &str,
) -> DomainResult<Vec<CanonicalTurnMutation>> {
    let mut mutations = Vec::new();
    for turn in state
        .canonical_turns
        .iter()
        .filter(|turn| &turn.session_id == session && !turn.status.is_terminal())
    {
        // 转交后的 Agent 已脱离原主线生命周期；主线重启只收口来源链，
        // 转交 Agent 的 Task/Turn/交互由其新的拥有方继续管理。
        if resolve_agent_run_boundary(store, state, turn)?.is_some() {
            continue;
        }
        let mut current = turn;
        let mut seen = std::collections::HashSet::new();
        while let Some(binding) = current.agent_turn_binding()? {
            if !seen.insert(current.turn_id.as_str()) {
                return Err(invalid("Agent Turn 来源链循环"));
            }
            if binding.parent_turn_id == source_turn_id {
                let next =
                    prepare_terminal_agent_turn(state, turn, CanonicalTurnStatus::Interrupted)?;
                mutations.push(CanonicalTurnMutation {
                    previous: Some(turn.clone()),
                    next,
                });
                break;
            }
            current = state
                .canonical_turns
                .iter()
                .find(|parent| {
                    parent.session_id == turn.session_id && parent.turn_id == binding.parent_turn_id
                })
                .ok_or_else(|| invalid("Agent Turn 父轮缺失"))?;
        }
    }
    Ok(mutations)
}

/// 主轮指针只决定主线交互；独立子轮通过父链证明活跃或重启恢复来源。
/// 只有同一主轮仍处于 ready 恢复状态，才允许回答已中断子轮的原问题/审批。
pub(super) fn interaction_turn_is_current_or_recoverable(
    store: &SessionStore,
    state: &SessionStoreState,
    turn: &CanonicalTurn,
) -> DomainResult<bool> {
    require_active_session(state, &turn.session_id)?;
    // 独立 Agent 的当前交互由其运行拥有方决定，不要求 Session 仍选中原主轮。
    if turn.agent_turn_binding()?.is_some()
        && !turn.status.is_terminal()
        && turn.completed_at.is_none()
    {
        return Ok(validate_agent_turn_admission(store, state, turn).is_ok());
    }
    if let Some((boundary_turn, transfer)) = resolve_agent_run_boundary(store, state, turn)? {
        // Interrupted 只允许处理原交互，不得借显式 Continue 越过已撤销的拥有方。
        validate_transferred_owner(state, &transfer)?;
        let mut current = turn;
        let mut seen = std::collections::HashSet::new();
        loop {
            if !seen.insert(current.turn_id.as_str())
                || current.status != CanonicalTurnStatus::Interrupted
            {
                return Ok(false);
            }
            if current.turn_id == boundary_turn {
                // TaskStore 的原转交与当前拥有方证明恢复范围，不能要求 Session
                // 仍选中原主轮。这里只允许答复原持久交互，不重新开放执行权。
                return Ok(true);
            }
            let binding = current
                .agent_turn_binding()?
                .ok_or_else(|| invalid("中断 Agent 恢复来源缺失"))?;
            current = state
                .canonical_turns
                .iter()
                .find(|parent| {
                    parent.session_id == turn.session_id && parent.turn_id == binding.parent_turn_id
                })
                .ok_or_else(|| invalid("中断 Agent 恢复父轮缺失"))?;
        }
    }
    let Some(current_turn) = state
        .execution_sidecar_store
        .runtime_sidecars
        .iter()
        .find(|sidecar| sidecar.session_id == turn.session_id)
        .and_then(|sidecar| sidecar.current_turn.as_ref())
    else {
        return Ok(false);
    };
    if current_turn.turn_id == turn.turn_id {
        return Ok(true);
    }
    if turn.agent_turn_binding()?.is_none() {
        return Ok(false);
    }
    if turn.status != CanonicalTurnStatus::Interrupted {
        return Ok(false);
    }
    let mut source = turn;
    let mut seen = std::collections::HashSet::new();
    while let Some(binding) = source.agent_turn_binding()? {
        if !seen.insert(source.turn_id.as_str()) {
            return Err(invalid("Agent Turn 来源链循环"));
        }
        source = state
            .canonical_turns
            .iter()
            .find(|parent| {
                parent.session_id == turn.session_id && parent.turn_id == binding.parent_turn_id
            })
            .ok_or_else(|| invalid("Agent Turn 父轮缺失"))?;
    }
    Ok(source.turn_id == current_turn.turn_id
        && source.status == CanonicalTurnStatus::Interrupted
        && source.items.iter().any(|item| {
            item.metadata.get("noticeKind").and_then(Value::as_str) == Some("session_interrupted")
                && item.metadata.get("recoveryState").and_then(Value::as_str) == Some("ready")
        }))
}
