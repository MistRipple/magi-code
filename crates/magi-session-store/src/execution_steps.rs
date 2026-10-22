//! Turn 内的执行步骤。唯一持久事实是 canonical system_notice metadata；不创建 Goal/Plan。
use crate::{ActiveExecutionTurnItem, CanonicalTurn, CanonicalTurnItem, CanonicalTurnItemStatus};
pub use magi_app_server_protocol::{
    ExecutionStep, ExecutionStepInput, ExecutionStepStatus, ExecutionSteps,
    UpdateExecutionStepsParams,
};
use magi_core::{DomainError, DomainResult, UtcMillis};
use std::collections::HashSet;
pub const EXECUTION_STEPS_KEY: &str = "executionSteps";
pub const EXECUTION_STEP_KEY: &str = "executionStepId";
fn invalid(message: impl Into<String>) -> DomainError {
    DomainError::Validation {
        message: message.into(),
    }
}
pub fn read_execution_steps(item: &CanonicalTurnItem) -> DomainResult<Option<ExecutionSteps>> {
    item.metadata
        .get(EXECUTION_STEPS_KEY)
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| invalid(format!("执行步骤数据无效: {error}")))
        })
        .transpose()
}
pub(crate) fn validate_steps_update(
    next: &CanonicalTurnItem,
    old: &CanonicalTurnItem,
) -> DomainResult<()> {
    let old_steps = read_execution_steps(old)?;
    let next_steps = read_execution_steps(next)?;
    let Some(old_steps) = old_steps else {
        return Ok(());
    };
    let next_steps = next_steps.ok_or_else(|| invalid("不能删除执行步骤事实"))?;
    if next_steps == old_steps {
        return Ok(());
    }
    if old_steps.execution_id != next_steps.execution_id
        || old_steps.task_id != next_steps.task_id
        || old_steps.turn_id != next_steps.turn_id
        || old_steps.created_at != next_steps.created_at
        || old_steps.revision.checked_add(1) != Some(next_steps.revision)
        || next_steps.updated_at < old_steps.updated_at
    {
        return Err(invalid("执行步骤身份或 revision 冲突"));
    }
    for step in &old_steps.steps {
        let next = next_steps
            .steps
            .iter()
            .find(|s| s.step_id == step.step_id)
            .ok_or_else(|| invalid("不能删除执行步骤"))?;
        if ended(&step.status) && next != step {
            return Err(invalid("不能改写已结束执行步骤"));
        }
    }
    Ok(())
}

pub fn unfinished(steps: &ExecutionSteps) -> bool {
    steps.steps.iter().any(|s| {
        matches!(
            s.status,
            ExecutionStepStatus::Pending | ExecutionStepStatus::Running
        )
    })
}
fn ended(status: &ExecutionStepStatus) -> bool {
    matches!(
        status,
        ExecutionStepStatus::Completed
            | ExecutionStepStatus::Failed
            | ExecutionStepStatus::Canceled
    )
}
fn unique(values: &[String]) -> bool {
    values.len() <= 64
        && values.iter().all(|v| !v.trim().is_empty())
        && values.iter().collect::<HashSet<_>>().len() == values.len()
}

pub(crate) fn prepare_steps(
    turn: &CanonicalTurn,
    task_id: &str,
    mut input: UpdateExecutionStepsParams,
    old: Option<&ExecutionSteps>,
) -> DomainResult<ExecutionSteps> {
    let now = UtcMillis::now().0.max(old.map_or(0, |s| s.updated_at));
    // 对外允许模型使用 toolCallId；持久证据统一保存 canonical itemId，历史分页可精确取回。
    for step in &mut input.steps {
        for reference in &mut step.evidence_refs {
            if let Some(item) = turn.items.iter().find(|i| {
                i.item_id == *reference || i.tool.as_ref().is_some_and(|t| t.call_id == *reference)
            }) {
                *reference = item.item_id.clone();
            }
        }
    }
    if input.expected_revision != old.map_or(0, |s| s.revision) {
        return Err(invalid(
            "execution_steps_revision_conflict: 使用 current_steps.revision 重试",
        ));
    }
    if !(2..=12).contains(&input.steps.len())
        || input
            .explanation
            .as_ref()
            .is_some_and(|s| s.chars().count() > 1000)
    {
        return Err(invalid("执行步骤必须为 2–12 项，说明不超过 1000 字"));
    }
    let ids = input
        .steps
        .iter()
        .map(|s| s.step_id.clone())
        .collect::<Vec<_>>();
    if !unique(&ids) || ids.iter().any(|id| id.len() > 120) {
        return Err(invalid("stepId 必须唯一且非空，最多 120 字节"));
    }
    if let Some(old) = old {
        if old.task_id != task_id || old.turn_id != turn.turn_id {
            return Err(invalid("执行步骤归属冲突"));
        }
        if old.steps.iter().any(|s| !ids.contains(&s.step_id)) {
            return Err(invalid("不能删除旧步骤；范围变化应保留并取消旧步骤"));
        }
    }
    let mut steps = Vec::new();
    for input_step in &input.steps {
        let s = input_step;
        if s.title.trim().is_empty()
            || s.title.chars().count() > 120
            || !unique(&s.depends_on)
            || !unique(&s.evidence_refs)
            || !unique(&s.task_ids)
            || s.reason.as_ref().is_some_and(|v| v.chars().count() > 1000)
        {
            return Err(invalid("步骤标题、引用或原因无效"));
        }
        let prior = old.and_then(|old| old.steps.iter().find(|p| p.step_id == s.step_id));
        if let Some(p) = prior {
            if ended(&p.status)
                && (p.title != s.title
                    || p.status != s.status
                    || p.depends_on != s.depends_on
                    || p.evidence_refs != s.evidence_refs
                    || p.task_ids != s.task_ids
                    || p.reason != s.reason
                    || p.plan_item_id != s.plan_item_id)
            {
                return Err(invalid("已结束步骤不可改写；重试应追加新 stepId"));
            }
            if p.status != ExecutionStepStatus::Running
                && matches!(
                    s.status,
                    ExecutionStepStatus::Completed | ExecutionStepStatus::Failed
                )
                && p.status != s.status
            {
                return Err(invalid("步骤必须先进入 running 才能完成或失败"));
            }
        } else if !matches!(
            s.status,
            ExecutionStepStatus::Pending | ExecutionStepStatus::Running
        ) {
            return Err(invalid("新步骤只能为 pending 或 running"));
        }
        if prior.is_some_and(|p| p.status == ExecutionStepStatus::Running)
            && s.status == ExecutionStepStatus::Pending
        {
            return Err(invalid("运行中步骤不能退回 pending"));
        }
        if matches!(
            s.status,
            ExecutionStepStatus::Blocked
                | ExecutionStepStatus::Failed
                | ExecutionStepStatus::Canceled
        ) && s.reason.as_ref().is_none_or(|v| v.trim().is_empty())
        {
            return Err(invalid("受阻、失败、取消必须提供原因"));
        }
        for dep in &s.depends_on {
            let index = ids
                .iter()
                .position(|id| id == dep)
                .ok_or_else(|| invalid("依赖步骤不存在"))?;
            let own = ids
                .iter()
                .position(|id| id == &s.step_id)
                .expect("own step");
            if index >= own {
                return Err(invalid("依赖必须指向前序步骤，不能自引用或循环"));
            }
            if matches!(
                s.status,
                ExecutionStepStatus::Running | ExecutionStepStatus::Completed
            ) && input.steps[index].status != ExecutionStepStatus::Completed
            {
                return Err(invalid("前置步骤未完成"));
            }
        }
        for reference in &s.evidence_refs {
            let evidence = turn
                .items
                .iter()
                .find(|i| {
                    i.item_id == *reference
                        || i.tool.as_ref().is_some_and(|t| t.call_id == *reference)
                })
                .ok_or_else(|| invalid("证据必须引用当前 Turn 的真实 itemId/toolCallId"))?;
            if evidence
                .worker
                .as_ref()
                .and_then(|w| w.task_id.as_ref())
                .map(|id| id.as_str())
                != Some(task_id)
                || evidence.metadata.contains_key(EXECUTION_STEPS_KEY)
                || evidence
                    .tool
                    .as_ref()
                    .is_some_and(|t| t.name == "update_execution_steps")
            {
                return Err(invalid("不能引用其他任务或步骤更新自身作为证据"));
            }
            if !matches!(
                evidence.kind,
                crate::CanonicalTurnItemKind::ToolCall
                    | crate::CanonicalTurnItemKind::AssistantText
            ) {
                return Err(invalid("思考、系统状态不能作为完成证据"));
            }
            if let Some(bound) = evidence
                .metadata
                .get(EXECUTION_STEP_KEY)
                .and_then(serde_json::Value::as_str)
                && bound != s.step_id
            {
                return Err(invalid("证据属于其他执行步骤"));
            }
            if prior
                .and_then(|p| p.started_at)
                .is_some_and(|started| evidence.updated_at.0 < started)
            {
                return Err(invalid("不能用步骤开始前的历史记录证明完成"));
            }
            if !evidence.status.is_terminal() {
                return Err(invalid("证据仍在执行，不能据此结算步骤"));
            }
            if s.status == ExecutionStepStatus::Completed
                && evidence.status != CanonicalTurnItemStatus::Completed
            {
                return Err(invalid("失败、取消或结果未知的工具不能证明步骤完成"));
            }
        }
        if s.status == ExecutionStepStatus::Completed
            && s.evidence_refs.is_empty()
            && s.task_ids.is_empty()
        {
            return Err(invalid(
                "完成步骤必须引用真实成功执行证据或已完成的直接子任务",
            ));
        }
        if let Some(plan_id) = &s.plan_item_id {
            if plan_id.trim().is_empty() {
                return Err(invalid("planItemId 不能为空"));
            }
        }
        steps.push(ExecutionStep {
            step_id: s.step_id.clone(),
            title: s.title.trim().into(),
            status: s.status.clone(),
            depends_on: s.depends_on.clone(),
            evidence_refs: s.evidence_refs.clone(),
            task_ids: s.task_ids.clone(),
            reason: s.reason.clone(),
            plan_item_id: s.plan_item_id.clone(),
            started_at: prior
                .and_then(|p| p.started_at)
                .or_else(|| (s.status == ExecutionStepStatus::Running).then_some(now)),
            settled_at: prior
                .and_then(|p| p.settled_at)
                .or_else(|| ended(&s.status).then_some(now)),
        });
    }
    let running = steps
        .iter()
        .filter(|s| s.status == ExecutionStepStatus::Running)
        .collect::<Vec<_>>();
    if running.len() > 1
        && (running.iter().any(|s| s.task_ids.is_empty())
            || running
                .iter()
                .flat_map(|s| &s.task_ids)
                .collect::<HashSet<_>>()
                .len()
                != running.iter().map(|s| s.task_ids.len()).sum::<usize>())
    {
        return Err(invalid("并行步骤必须绑定互不重叠的直接子任务"));
    }
    Ok(ExecutionSteps {
        execution_id: old
            .map(|s| s.execution_id.clone())
            .unwrap_or_else(|| format!("execution-steps:{}:{}", turn.turn_id, task_id)),
        turn_id: turn.turn_id.clone(),
        task_id: task_id.into(),
        revision: input
            .expected_revision
            .checked_add(1)
            .ok_or_else(|| invalid("执行步骤 revision 溢出"))?,
        steps,
        explanation: input.explanation,
        created_at: old.map_or(now, |s| s.created_at),
        updated_at: now,
    })
}

/// Turn 终态结算嵌套步骤；取消/重启不将未执行工作伪造为完成。
pub(crate) fn settle_steps(item: &mut ActiveExecutionTurnItem, terminal: &str) {
    let Some(value) = item.metadata.get(EXECUTION_STEPS_KEY) else {
        return;
    };
    let Ok(mut snapshot) = serde_json::from_value::<ExecutionSteps>(value.clone()) else {
        return;
    };
    let mut changed = false;
    for step in &mut snapshot.steps {
        if !ended(&step.status) && step.status != ExecutionStepStatus::Blocked {
            step.status = if terminal == "failed" && step.status == ExecutionStepStatus::Running {
                ExecutionStepStatus::Failed
            } else {
                ExecutionStepStatus::Canceled
            };
            step.reason = Some(
                match terminal {
                    "failed" => "所属任务失败，步骤未完成",
                    "completed" => "任务已结束，步骤未确认完成",
                    _ => "所属任务已停止，步骤未完成",
                }
                .into(),
            );
            step.settled_at = Some(UtcMillis::now().0);
            changed = true;
        }
    }
    if changed {
        snapshot.revision += 1;
        snapshot.updated_at = UtcMillis::now().0;
        item.metadata.insert(
            EXECUTION_STEPS_KEY.into(),
            serde_json::to_value(snapshot).expect("steps serializable"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CanonicalToolCall, CanonicalTurnItemKind, CanonicalTurnVisibility, CanonicalWorkerRef,
    };
    use magi_core::{SessionId, TaskId, ThreadId};
    use serde_json::json;
    use std::collections::HashMap;

    fn turn() -> CanonicalTurn {
        let task = TaskId::new("task");
        let mut metadata = HashMap::new();
        metadata.insert("requestId".into(), json!("request"));
        CanonicalTurn {
            session_id: SessionId::new("session"),
            turn_id: "turn".into(),
            turn_seq: 1,
            accepted_at: UtcMillis(1),
            completed_at: None,
            status: crate::CanonicalTurnStatus::Running,
            response_duration_ms: None,
            usage: None,
            items: vec![CanonicalTurnItem {
                session_id: SessionId::new("session"),
                turn_id: "turn".into(),
                turn_seq: 1,
                item_id: "tool-item".into(),
                item_seq: 1,
                kind: CanonicalTurnItemKind::ToolCall,
                created_at: UtcMillis(1),
                status: CanonicalTurnItemStatus::Completed,
                item_version: Some(1),
                updated_at: UtcMillis(2),
                title: None,
                content: None,
                blocks: vec![],
                tool: Some(CanonicalToolCall {
                    call_id: "call".into(),
                    name: "file_read".into(),
                    arguments: None,
                    result: Some(json!({"status":"succeeded"})),
                    error: None,
                }),
                worker: Some(CanonicalWorkerRef {
                    task_id: Some(task),
                    worker_id: None,
                    role_id: None,
                    title: None,
                }),
                source_thread_id: ThreadId::new("thread"),
                visibility: CanonicalTurnVisibility::default(),
                metadata,
            }],
            model_steps: vec![],
            metadata: HashMap::new(),
        }
    }

    fn input(status: ExecutionStepStatus) -> ExecutionStepInput {
        ExecutionStepInput {
            step_id: "one".into(),
            title: "读取资料".into(),
            status,
            depends_on: vec![],
            evidence_refs: vec![],
            task_ids: vec![],
            reason: None,
            plan_item_id: None,
        }
    }

    #[test]
    fn new_execution_steps_accept_pending_and_running_only() {
        let t = turn();
        let params = UpdateExecutionStepsParams {
            expected_revision: 0,
            steps: vec![
                input(ExecutionStepStatus::Running),
                ExecutionStepInput {
                    step_id: "two".into(),
                    title: "验证".into(),
                    status: ExecutionStepStatus::Pending,
                    depends_on: vec!["one".into()],
                    evidence_refs: vec![],
                    task_ids: vec![],
                    reason: None,
                    plan_item_id: None,
                },
            ],
            explanation: None,
        };
        let snapshot = prepare_steps(&t, "task", params, None).expect("valid steps");
        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.steps[0].status, ExecutionStepStatus::Running);
    }

    #[test]
    fn completed_step_requires_success_evidence_and_running_transition() {
        let t = turn();
        let params = UpdateExecutionStepsParams {
            expected_revision: 0,
            steps: vec![
                ExecutionStepInput {
                    status: ExecutionStepStatus::Completed,
                    ..input(ExecutionStepStatus::Completed)
                },
                ExecutionStepInput {
                    step_id: "two".into(),
                    title: "验证".into(),
                    status: ExecutionStepStatus::Pending,
                    depends_on: vec![],
                    evidence_refs: vec![],
                    task_ids: vec![],
                    reason: None,
                    plan_item_id: None,
                },
            ],
            explanation: None,
        };
        assert!(prepare_steps(&t, "task", params, None).is_err());
    }
}
