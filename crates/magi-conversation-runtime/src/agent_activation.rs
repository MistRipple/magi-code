//! 可持续 Agent 沿现有 Task/Thread 演进；本模块不持有另一份任务状态或父子树。
use magi_core::{AgentActivation, AgentLifetime, SessionId, Task, TaskId, TaskStatus};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{GoalStatus, SessionStore};

/// 会话保留的可继续身份；目录和下一轮模型上下文使用同一投影。
pub(crate) fn retained_agent_heads(
    task_store: &TaskStore,
    session_store: &SessionStore,
    session_id: &SessionId,
    parent: &Task,
) -> Vec<Task> {
    let goal = session_store.current_unfinished_goal(session_id);
    task_store
        .agent_heads_for_session(session_id)
        .into_iter()
        .filter(|task| {
            task_store.agent_resume_parent_matches(task, parent)
                && task
                    .agent_activation()
                    .is_some_and(|binding| match &binding.lifetime {
                        AgentLifetime::Turn => false,
                        AgentLifetime::Session => true,
                        AgentLifetime::Goal { goal_id } => goal.as_ref().is_some_and(|goal| {
                            goal.goal_id == *goal_id && goal.status == GoalStatus::Active
                        }),
                    })
        })
        .collect()
}

pub(crate) struct ActivationPreflight<'a> {
    pub task_store: &'a TaskStore,
    pub session_store: &'a SessionStore,
    pub session_id: &'a SessionId,
    pub parent: &'a Task,
    pub role: &'a str,
    pub capabilities: &'a [String],
    pub lifetime: Option<&'a str>,
    pub previous_task_id: Option<&'a str>,
}

pub(crate) fn preflight_activation(
    input: ActivationPreflight<'_>,
) -> Result<(AgentLifetime, Option<AgentActivation>), String> {
    let ActivationPreflight {
        task_store,
        session_store,
        session_id,
        parent,
        role,
        capabilities,
        lifetime,
        previous_task_id,
    } = input;
    let active_goal = || {
        session_store
            .active_execution_chain(session_id)
            .and_then(|chain| chain.current_turn)
            .and_then(|turn| session_store.model_step_goal(session_id, &turn.turn_id))
            .filter(|goal| goal.status == GoalStatus::Active)
    };
    let requested_lifetime = match lifetime {
        None => None,
        Some("turn") => Some(AgentLifetime::Turn),
        Some("session") => Some(AgentLifetime::Session),
        Some("goal") => {
            let goal = active_goal().ok_or("Goal 生命周期需要当前 Turn 所属的活跃 Goal")?;
            Some(AgentLifetime::Goal {
                goal_id: goal.goal_id,
            })
        }
        Some(_) => return Err("lifetime 只能为 turn、session 或 goal".into()),
    };
    let Some(previous_id) = previous_task_id else {
        return Ok((requested_lifetime.unwrap_or_default(), None));
    };
    let source = task_store
        .get_task(&TaskId::new(previous_id))
        .ok_or("来源 activation 不存在")?;
    let binding = source
        .agent_activation()
        .ok_or("来源任务没有持久 Agent 身份，请创建新代理")?;
    if binding.session_id != *session_id
        || source.mission_id != parent.mission_id
        || source.parent_task_id.is_none()
        || !task_store.agent_resume_parent_matches(&source, parent)
        || source.workspace_scope != parent.workspace_scope
        || source.write_scope != parent.write_scope
        || source.executor_binding_target_role() != Some(role)
        || capabilities.iter().any(|capability| {
            !source
                .executor_binding_capability_ids()
                .contains(capability)
        })
        || source.status != TaskStatus::Completed
        || matches!(binding.lifetime, AgentLifetime::Turn)
        || requested_lifetime
            .as_ref()
            .is_some_and(|requested| requested != &binding.lifetime)
    {
        return Err("来源必须是同一会话与角色下、已完成且保留到 Session/Goal 的 activation；不能改变归属或扩大能力".into());
    }
    if let AgentLifetime::Goal { goal_id } = &binding.lifetime
        && active_goal().is_none_or(|goal| goal.goal_id != *goal_id)
    {
        return Err("来源 Agent 所属 Goal 已结束或暂停".into());
    }
    if task_store
        .agent_heads_for_session(session_id)
        .iter()
        .find(|head| {
            head.agent_activation()
                .is_some_and(|candidate| candidate.agent_id == binding.agent_id)
        })
        .is_none_or(|head| head.task_id != source.task_id)
    {
        return Err("来源不是 Agent 的最新 activation，请使用最新 task_id".into());
    }
    if !session_store
        .thread_registry_snapshot(session_id)
        .iter()
        .any(|thread| thread.handled_task_ids.contains(&source.task_id))
    {
        return Err("来源 Agent 的持久 Thread 缺失".into());
    }
    // 新 activation 的授权来自当前父任务；历史 Task/Thread 只贡献上下文，
    // 不能把旧权限、批准或执行租约带入本轮。
    Ok((binding.lifetime.clone(), Some(binding.clone())))
}

/// 仅授予同一 Agent 的明确前序 activation；不扩大到旧父任务、兄弟或旧整棵树。
pub(crate) fn previous_activation_tasks(
    task_store: &TaskStore,
    task: &Task,
    session: &SessionId,
) -> Result<Vec<Task>, String> {
    let Some(identity) = task.agent_activation() else {
        return Ok(Vec::new());
    };
    if identity.session_id != *session {
        return Err("Agent activation 会话归属不匹配".into());
    }
    let mut previous = identity.previous_task_id.clone();
    let mut generation = identity.generation;
    let mut result = Vec::new();
    while let Some(id) = previous {
        let source = task_store
            .get_task(&id)
            .ok_or("Agent 前序 activation 缺失")?;
        let binding = source
            .agent_activation()
            .ok_or("Agent 前序 activation 没有身份")?;
        if binding.agent_id != identity.agent_id
            || binding.session_id != *session
            || binding.generation.checked_add(1) != Some(generation)
            || binding.lifetime != identity.lifetime
            || source.mission_id != task.mission_id
            || source.workspace_scope != task.workspace_scope
            || source.write_scope != task.write_scope
            || source.executor_binding_target_role() != task.executor_binding_target_role()
            || source.status != TaskStatus::Completed
        {
            return Err("Agent activation 来源或代际不匹配".into());
        }
        generation = binding.generation;
        previous = binding.previous_task_id.clone();
        result.push(source);
    }
    if generation != 1 {
        return Err("Agent activation 来源链不完整".into());
    }
    Ok(result)
}

pub(crate) fn previous_activation_ids(
    task_store: &TaskStore,
    task: &Task,
    session: &SessionId,
) -> Vec<TaskId> {
    previous_activation_tasks(task_store, task, session)
        .unwrap_or_default()
        .into_iter()
        .map(|task| task.task_id)
        .collect()
}
