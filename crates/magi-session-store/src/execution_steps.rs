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

/// 完成证据只由服务端派生：步骤 running 期间绑定到它、并已成功结束的工具调用或助手文字，取最近 64 条。
/// 模型只声明步骤状态，不提交也无法伪造条目引用。
fn bound_success_evidence(turn: &CanonicalTurn, task_id: &str, step_id: &str) -> Vec<String> {
    let mut refs = turn
        .items
        .iter()
        .filter(|item| {
            item.status == CanonicalTurnItemStatus::Completed
                && matches!(
                    item.kind,
                    crate::CanonicalTurnItemKind::ToolCall
                        | crate::CanonicalTurnItemKind::AssistantText
                )
                && item
                    .worker
                    .as_ref()
                    .and_then(|w| w.task_id.as_ref())
                    .map(|id| id.as_str())
                    == Some(task_id)
                && !item.metadata.contains_key(EXECUTION_STEPS_KEY)
                && item
                    .tool
                    .as_ref()
                    .is_none_or(|t| t.name != "update_execution_steps")
                && item
                    .metadata
                    .get(EXECUTION_STEP_KEY)
                    .and_then(serde_json::Value::as_str)
                    == Some(step_id)
        })
        .map(|item| item.item_id.clone())
        .collect::<Vec<_>>();
    if refs.len() > 64 {
        refs.drain(..refs.len() - 64);
    }
    refs
}

pub(crate) fn prepare_steps(
    turn: &CanonicalTurn,
    task_id: &str,
    input: UpdateExecutionStepsParams,
    old: Option<&ExecutionSteps>,
) -> DomainResult<ExecutionSteps> {
    let now = UtcMillis::now().0.max(old.map_or(0, |s| s.updated_at));
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
        let depends_on = s.depends_on.clone().unwrap_or_default();
        let task_ids = s.task_ids.clone().unwrap_or_default();
        if s.title.trim().is_empty()
            || s.title.chars().count() > 120
            || !unique(&depends_on)
            || !unique(&task_ids)
            || s.reason.as_ref().is_some_and(|v| v.chars().count() > 1000)
        {
            return Err(invalid("步骤标题、引用或原因无效"));
        }
        let prior = old.and_then(|old| old.steps.iter().find(|p| p.step_id == s.step_id));
        if let Some(p) = prior {
            if ended(&p.status)
                && (p.title != s.title
                    || p.status != s.status
                    || p.depends_on != depends_on
                    || p.task_ids != task_ids
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
        for dep in &depends_on {
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
        // 证据随状态派生：完成时取该步骤运行期间的成功条目；已结束步骤保持原证据；其余状态无证据。
        let evidence_refs = match (prior, &s.status) {
            (Some(p), _) if ended(&p.status) => p.evidence_refs.clone(),
            (_, ExecutionStepStatus::Completed) => {
                bound_success_evidence(turn, task_id, &s.step_id)
            }
            _ => Vec::new(),
        };
        if s.status == ExecutionStepStatus::Completed
            && evidence_refs.is_empty()
            && task_ids.is_empty()
        {
            return Err(invalid(
                "完成步骤前，该步骤必须在 running 期间有成功的工具调用或已完成的直接子任务；请先真正执行再标记完成",
            ));
        }
        if let Some(plan_id) = &s.plan_item_id
            && plan_id.trim().is_empty()
        {
            return Err(invalid("planItemId 不能为空"));
        }
        steps.push(ExecutionStep {
            step_id: s.step_id.clone(),
            title: s.title.trim().into(),
            status: s.status.clone(),
            depends_on,
            evidence_refs,
            task_ids,
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
        // revision 由服务端单调推进：步骤只有所属 Task 一个写者，模型不需要追踪版本号。
        revision: old
            .map_or(0, |s| s.revision)
            .checked_add(1)
            .ok_or_else(|| invalid("执行步骤 revision 溢出"))?,
        steps,
        explanation: input.explanation,
        created_at: old.map_or(now, |s| s.created_at),
        updated_at: now,
    })
}

/// 终态结算时的完成依据：(任务, 步骤, 条目)。由 [`collect_step_proof`] 从当前 Turn 的活动条目派生。
pub(crate) type StepProof = Vec<(String, String, String)>;

/// 绑定到某个步骤、且能证明该步骤确实做了事的条目：已成功的工具调用，以及助手文字。
/// 助手文字允许仍在流式中的条目——Turn 正常完成时，最终答复本身就是最后一步（如“总结”）的产出。
pub(crate) fn collect_step_proof(
    items: &[ActiveExecutionTurnItem],
    succeeded: impl Fn(&str) -> bool,
    active: impl Fn(&str) -> bool,
) -> StepProof {
    items
        .iter()
        .filter(|item| {
            if item.metadata.contains_key(EXECUTION_STEPS_KEY)
                || item.tool_name.as_deref() == Some("update_execution_steps")
            {
                return false;
            }
            match item.kind.as_str() {
                "assistant_stream" | "assistant_final" => {
                    succeeded(&item.status) || active(&item.status)
                }
                "tool_call_result" => {
                    succeeded(&item.status) && item.tool_status.as_deref().is_none_or(&succeeded)
                }
                _ => false,
            }
        })
        .filter_map(|item| {
            Some((
                item.task_id.as_ref()?.as_str().to_string(),
                item.metadata.get(EXECUTION_STEP_KEY)?.as_str()?.to_string(),
                item.item_id.clone(),
            ))
        })
        .collect()
}

/// Turn 终态结算嵌套步骤。步骤状态不靠模型在最终答复前补记：
/// 正常完成时，有成功依据的 running 步骤算完成，其余 running/pending 标为未确认/未执行；
/// 失败、取消不将未执行工作伪造为完成。
pub(crate) fn settle_steps(item: &mut ActiveExecutionTurnItem, terminal: &str, proof: &StepProof) {
    let Some(value) = item.metadata.get(EXECUTION_STEPS_KEY) else {
        return;
    };
    let Ok(mut snapshot) = serde_json::from_value::<ExecutionSteps>(value.clone()) else {
        return;
    };
    let mut changed = false;
    for step in &mut snapshot.steps {
        if ended(&step.status) || step.status == ExecutionStepStatus::Blocked {
            continue;
        }
        let proven = (terminal == "completed" && step.status == ExecutionStepStatus::Running)
            .then(|| {
                let mut refs = proof
                    .iter()
                    .filter(|(task, step_id, _)| {
                        *task == snapshot.task_id && *step_id == step.step_id
                    })
                    .map(|(_, _, item_id)| item_id.clone())
                    .collect::<Vec<_>>();
                if refs.len() > 64 {
                    refs.drain(..refs.len() - 64);
                }
                refs
            })
            .filter(|refs| !refs.is_empty());
        if let Some(refs) = proven {
            step.status = ExecutionStepStatus::Completed;
            step.evidence_refs = refs;
            step.reason = None;
        } else {
            let was_running = step.status == ExecutionStepStatus::Running;
            step.status = if terminal == "failed" && was_running {
                ExecutionStepStatus::Failed
            } else {
                ExecutionStepStatus::Canceled
            };
            step.reason = Some(
                match (terminal, was_running) {
                    ("failed", _) => "所属任务失败，步骤未完成",
                    ("completed", true) => "任务已结束，步骤未确认完成",
                    ("completed", false) => "任务已结束，步骤未执行",
                    _ => "所属任务已停止，步骤未完成",
                }
                .into(),
            );
        }
        step.settled_at = Some(UtcMillis::now().0);
        changed = true;
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
            depends_on: None,
            task_ids: None,
            reason: None,
            plan_item_id: None,
        }
    }

    #[test]
    fn new_execution_steps_accept_pending_and_running_only() {
        let t = turn();
        let params = UpdateExecutionStepsParams {
            steps: vec![
                input(ExecutionStepStatus::Running),
                ExecutionStepInput {
                    step_id: "two".into(),
                    title: "验证".into(),
                    status: ExecutionStepStatus::Pending,
                    depends_on: Some(vec!["one".into()]),
                    task_ids: None,
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
            steps: vec![
                ExecutionStepInput {
                    status: ExecutionStepStatus::Completed,
                    ..input(ExecutionStepStatus::Completed)
                },
                ExecutionStepInput {
                    step_id: "two".into(),
                    title: "验证".into(),
                    status: ExecutionStepStatus::Pending,
                    depends_on: None,
                    task_ids: None,
                    reason: None,
                    plan_item_id: None,
                },
            ],
            explanation: None,
        };
        assert!(prepare_steps(&t, "task", params, None).is_err());
    }

    fn two_steps(
        first: ExecutionStepStatus,
        second: ExecutionStepStatus,
    ) -> UpdateExecutionStepsParams {
        UpdateExecutionStepsParams {
            steps: vec![
                input(first),
                ExecutionStepInput {
                    step_id: "two".into(),
                    title: "验证".into(),
                    status: second,
                    depends_on: Some(vec!["one".into()]),
                    task_ids: None,
                    reason: None,
                    plan_item_id: None,
                },
            ],
            explanation: None,
        }
    }

    #[test]
    fn completed_step_adopts_evidence_bound_during_its_run() {
        let mut t = turn();
        let started = prepare_steps(
            &t,
            "task",
            two_steps(ExecutionStepStatus::Running, ExecutionStepStatus::Pending),
            None,
        )
        .expect("running");
        // 模型没有引用 ID：采用步骤运行期间绑定到它、已成功结束的工具调用。
        t.items[0]
            .metadata
            .insert(EXECUTION_STEP_KEY.into(), json!("one"));
        t.items[0].updated_at = UtcMillis(UtcMillis::now().0 + 10_000);
        let done = prepare_steps(
            &t,
            "task",
            two_steps(ExecutionStepStatus::Completed, ExecutionStepStatus::Running),
            Some(&started),
        )
        .expect("completed without explicit refs");
        assert_eq!(done.steps[0].evidence_refs, ["tool-item"]);
        // 已完成步骤省略引用原样重提交，不算改写。
        let again = prepare_steps(
            &t,
            "task",
            two_steps(ExecutionStepStatus::Completed, ExecutionStepStatus::Running),
            Some(&done),
        );
        assert!(again.is_ok(), "{again:?}");
    }

    #[test]
    fn completed_step_without_bound_evidence_is_still_rejected() {
        let t = turn();
        let started = prepare_steps(
            &t,
            "task",
            two_steps(ExecutionStepStatus::Running, ExecutionStepStatus::Pending),
            None,
        )
        .expect("running");
        // 条目没有绑定到该步骤：不能凭空完成。
        assert!(
            prepare_steps(
                &t,
                "task",
                two_steps(ExecutionStepStatus::Completed, ExecutionStepStatus::Running),
                Some(&started),
            )
            .is_err()
        );
    }

    #[test]
    fn model_cannot_submit_evidence_and_foreign_step_items_are_not_evidence() {
        // 证据由服务端派生，输入里出现 evidenceRefs 直接被协议拒绝。
        assert!(
            serde_json::from_value::<ExecutionStepInput>(json!({
                "stepId": "one", "title": "读取", "status": "completed",
                "dependsOn": [], "evidenceRefs": ["tool-item"], "taskIds": []
            }))
            .is_err()
        );
        // 绑定到其他步骤的条目不能为当前步骤作证。
        let mut t = turn();
        let started = prepare_steps(
            &t,
            "task",
            two_steps(ExecutionStepStatus::Running, ExecutionStepStatus::Pending),
            None,
        )
        .expect("running");
        t.items[0]
            .metadata
            .insert(EXECUTION_STEP_KEY.into(), json!("two"));
        t.items[0].updated_at = UtcMillis(UtcMillis::now().0 + 10_000);
        assert!(
            prepare_steps(
                &t,
                "task",
                two_steps(ExecutionStepStatus::Completed, ExecutionStepStatus::Running),
                Some(&started),
            )
            .is_err()
        );
    }
}
