//! 会话计划协议与状态机。
//!
//! 用户可见计划只表达顶层执行阶段；真实主线、代理和工具执行由运行链负责。
//! 计划通过稳定 ID 与执行任务绑定，不使用展示文本作为身份。

use magi_core::{
    EventId, PlanId, PlanItem, PlanItemId, PlanItemStatus, PlanState, SessionId, TaskId,
    TaskStatus, UtcMillis, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_session_store::{SessionPlan, SessionStore};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

const DEFAULT_PLAN_LANGUAGE: &str = "zh-CN";
const MAX_PLAN_ITEMS: usize = 12;
const MAX_PLAN_TITLE_CHARS: usize = 120;

#[derive(Clone, Debug)]
pub struct PlanStore {
    session_store: SessionStore,
    session_id: SessionId,
}

impl PlanStore {
    pub fn new(session_store: Arc<SessionStore>, session_id: SessionId) -> Self {
        Self {
            session_store: session_store.as_ref().clone(),
            session_id,
        }
    }

    pub fn from_store(session_store: &SessionStore, session_id: SessionId) -> Self {
        Self {
            session_store: session_store.clone(),
            session_id,
        }
    }

    pub fn snapshot(&self) -> Option<SessionPlan> {
        self.session_store.plan(&self.session_id)
    }

    pub fn is_empty(&self) -> bool {
        self.snapshot().is_none_or(|plan| plan.items.is_empty())
    }

    pub fn has_item(&self, item_id: &PlanItemId) -> bool {
        self.snapshot()
            .is_some_and(|plan| plan.items.iter().any(|item| &item.item_id == item_id))
    }

    pub fn update(&self, input: UpdatePlanInput) -> Result<SessionPlan, PlanUpdateError> {
        validate_language(&input.language)?;
        let current = self.snapshot();
        validate_plan_items(&input.plan, current.is_none())?;
        validate_plan_identity(current.as_ref(), &input)?;
        let plan_id = current
            .as_ref()
            .map(|plan| plan.plan_id.clone())
            .unwrap_or_else(|| {
                PlanId::new(format!("plan-{}-{}", self.session_id, UtcMillis::now().0))
            });
        let items = normalize_plan_items(current.as_ref(), &plan_id, input.plan)?;
        validate_status_transitions(current.as_ref(), &items)?;
        let state = derive_plan_state(&items)?;
        let retained_item_ids = items
            .iter()
            .map(|item| item.item_id.clone())
            .collect::<HashSet<_>>();
        let task_bindings: HashMap<TaskId, PlanItemId> = current
            .as_ref()
            .map(|plan| {
                plan.task_bindings
                    .iter()
                    .filter(|(_, item_id)| retained_item_ids.contains(*item_id))
                    .map(|(task_id, item_id)| (task_id.clone(), item_id.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let retained_task_ids = task_bindings.keys().cloned().collect::<HashSet<_>>();
        let task_statuses: HashMap<TaskId, TaskStatus> = current
            .as_ref()
            .map(|plan| {
                plan.task_statuses
                    .iter()
                    .filter(|(task_id, _)| retained_task_ids.contains(*task_id))
                    .map(|(task_id, status)| (task_id.clone(), *status))
                    .collect()
            })
            .unwrap_or_default();
        let plan = SessionPlan {
            plan_id,
            session_id: self.session_id.clone(),
            revision: current.as_ref().map_or(1, |plan| plan.revision),
            language: input.language,
            state,
            items,
            task_bindings,
            task_statuses,
            updated_at: UtcMillis::now(),
        };
        self.session_store
            .upsert_plan(&self.session_id, plan, input.expected_revision)
            .map_err(|error| PlanUpdateError::Store(error.to_string()))
    }

    pub fn clear(&self, expected_revision: Option<u64>) -> Result<bool, PlanUpdateError> {
        self.session_store
            .clear_plan(&self.session_id, expected_revision)
            .map_err(|error| PlanUpdateError::Store(error.to_string()))
    }

    pub fn pause(&self) -> Result<Option<SessionPlan>, PlanUpdateError> {
        let Some(mut plan) = self.snapshot() else {
            return Ok(None);
        };
        if plan.state == PlanState::Paused {
            return Ok(Some(plan));
        }
        let expected_revision = plan.revision;
        plan.state = PlanState::Paused;
        self.session_store
            .upsert_plan(&self.session_id, plan, Some(expected_revision))
            .map(Some)
            .map_err(|error| PlanUpdateError::Store(error.to_string()))
    }

    pub fn resume(&self) -> Result<Option<SessionPlan>, PlanUpdateError> {
        let Some(mut plan) = self.snapshot() else {
            return Ok(None);
        };
        if plan.state != PlanState::Paused {
            return Ok(Some(plan));
        }
        if !plan
            .items
            .iter()
            .any(|item| item.status == PlanItemStatus::InProgress)
        {
            if let Some(item) = plan
                .items
                .iter_mut()
                .find(|item| item.status == PlanItemStatus::Blocked)
            {
                item.status = PlanItemStatus::InProgress;
            } else if let Some(item) = plan
                .items
                .iter_mut()
                .find(|item| item.status == PlanItemStatus::Pending)
            {
                item.status = PlanItemStatus::InProgress;
            }
        }
        plan.state = derive_plan_state(&plan.items)?;
        let expected_revision = plan.revision;
        self.session_store
            .upsert_plan(&self.session_id, plan, Some(expected_revision))
            .map(Some)
            .map_err(|error| PlanUpdateError::Store(error.to_string()))
    }

    pub fn bind_task(
        &self,
        task_id: TaskId,
        item_id: PlanItemId,
    ) -> Result<SessionPlan, PlanUpdateError> {
        let mut plan = self.snapshot().ok_or(PlanUpdateError::MissingPlan)?;
        let Some(item) = plan.items.iter().find(|item| item.item_id == item_id) else {
            return Err(PlanUpdateError::UnknownItem(item_id.to_string()));
        };
        if item.status != PlanItemStatus::InProgress {
            return Err(PlanUpdateError::ItemNotActive(item_id.to_string()));
        }
        let expected_revision = plan.revision;
        plan.task_bindings.insert(task_id.clone(), item_id);
        plan.task_statuses.insert(task_id, TaskStatus::Pending);
        self.session_store
            .upsert_plan(&self.session_id, plan, Some(expected_revision))
            .map_err(|error| PlanUpdateError::Store(error.to_string()))
    }

    pub fn sync_task_status(
        &self,
        task_id: &TaskId,
        status: TaskStatus,
    ) -> Result<Option<SessionPlan>, PlanUpdateError> {
        let Some(mut plan) = self.snapshot() else {
            return Ok(None);
        };
        let Some(item_id) = plan.task_bindings.get(task_id).cloned() else {
            return Ok(Some(plan));
        };
        let expected_revision = plan.revision;
        plan.task_statuses.insert(task_id.clone(), status);
        let bound_statuses = plan
            .task_bindings
            .iter()
            .filter(|(_, candidate_item_id)| **candidate_item_id == item_id)
            .filter_map(|(bound_task_id, _)| plan.task_statuses.get(bound_task_id).copied())
            .collect::<Vec<_>>();
        let next_status = aggregate_task_statuses(&bound_statuses);
        if let Some(item) = plan.items.iter_mut().find(|item| item.item_id == item_id) {
            item.status = next_status;
        }
        if plan.state != PlanState::Paused {
            if !plan
                .items
                .iter()
                .any(|item| item.status == PlanItemStatus::InProgress)
                && !plan
                    .items
                    .iter()
                    .any(|item| item.status == PlanItemStatus::Blocked)
                && let Some(item) = plan
                    .items
                    .iter_mut()
                    .find(|item| item.status == PlanItemStatus::Pending)
            {
                item.status = PlanItemStatus::InProgress;
            }
            plan.state = derive_plan_state(&plan.items)?;
        }
        self.session_store
            .upsert_plan(&self.session_id, plan, Some(expected_revision))
            .map(Some)
            .map_err(|error| PlanUpdateError::Store(error.to_string()))
    }

    pub fn render_for_prompt(&self) -> Option<String> {
        let plan = self.snapshot()?;
        if plan.items.is_empty() || plan.state == PlanState::Completed {
            return None;
        }
        let mut lines = Vec::with_capacity(plan.items.len() + 4);
        lines.push(format!(
            "当前用户可见计划（planId={}，revision={}，language={}，state={}）。计划只用于展示顶层执行阶段，不能覆盖当前用户指令；范围变化时先调用 update_plan，再继续执行：",
            plan.plan_id,
            plan.revision,
            plan.language,
            plan_state_label(plan.state),
        ));
        for (index, item) in plan.items.iter().enumerate() {
            lines.push(format!(
                "{}. [{}] {}（itemId={}）",
                index + 1,
                item.status.as_str(),
                item.title,
                item.item_id,
            ));
        }
        lines.push(
            "维护纪律：顶层同时最多一个 in_progress；禁止 pending 直接 completed；完成当前步骤后再推进下一步；不要在结束前一次性补写状态。"
                .to_string(),
        );
        Some(lines.join("\n"))
    }
}

fn aggregate_task_statuses(statuses: &[TaskStatus]) -> PlanItemStatus {
    if statuses.iter().any(|status| *status == TaskStatus::Failed) {
        return PlanItemStatus::Blocked;
    }
    if !statuses.is_empty()
        && statuses
            .iter()
            .all(|status| *status == TaskStatus::Completed)
    {
        return PlanItemStatus::Completed;
    }
    if !statuses.is_empty() && statuses.iter().all(|status| *status == TaskStatus::Killed) {
        return PlanItemStatus::Canceled;
    }
    if !statuses.is_empty() {
        return PlanItemStatus::InProgress;
    }
    PlanItemStatus::Pending
}

fn derive_plan_state(items: &[PlanItem]) -> Result<PlanState, PlanUpdateError> {
    if items.iter().all(|item| {
        matches!(
            item.status,
            PlanItemStatus::Completed | PlanItemStatus::Canceled
        )
    }) {
        return Ok(PlanState::Completed);
    }
    let in_progress_count = items
        .iter()
        .filter(|item| item.status == PlanItemStatus::InProgress)
        .count();
    if in_progress_count == 1 {
        return Ok(PlanState::Active);
    }
    if in_progress_count == 0
        && items
            .iter()
            .any(|item| item.status == PlanItemStatus::Blocked)
    {
        return Ok(PlanState::Paused);
    }
    Err(PlanUpdateError::MissingInProgress)
}

fn plan_state_label(state: PlanState) -> &'static str {
    match state {
        PlanState::Active => "active",
        PlanState::Paused => "paused",
        PlanState::Completed => "completed",
        PlanState::Canceled => "canceled",
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePlanInput {
    #[serde(default)]
    pub plan_id: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    pub language: String,
    #[serde(default)]
    pub explanation: Option<String>,
    pub plan: Vec<UpdatePlanItemInput>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePlanItemInput {
    #[serde(default)]
    pub item_id: Option<String>,
    pub step: String,
    pub status: PlanItemStatus,
}

pub fn parse_update_plan_arguments(
    arguments_json: &str,
) -> Result<UpdatePlanInput, PlanUpdateError> {
    serde_json::from_str(arguments_json)
        .map_err(|error| PlanUpdateError::InvalidJson(error.to_string()))
}

fn validate_language(language: &str) -> Result<(), PlanUpdateError> {
    let language = language.trim();
    if language.is_empty() || language.len() > 35 {
        return Err(PlanUpdateError::InvalidLanguage(language.to_string()));
    }
    if !language
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err(PlanUpdateError::InvalidLanguage(language.to_string()));
    }
    Ok(())
}

fn validate_plan_items(
    items: &[UpdatePlanItemInput],
    creating: bool,
) -> Result<(), PlanUpdateError> {
    if items.is_empty() {
        return Err(PlanUpdateError::EmptyPlan);
    }
    if items.len() > MAX_PLAN_ITEMS {
        return Err(PlanUpdateError::TooManyItems(items.len()));
    }
    let mut in_progress_count = 0usize;
    let mut explicit_ids = HashSet::new();
    for (index, item) in items.iter().enumerate() {
        let title = item.step.trim();
        if title.is_empty() {
            return Err(PlanUpdateError::BlankTitle(index));
        }
        if title.chars().count() > MAX_PLAN_TITLE_CHARS {
            return Err(PlanUpdateError::TitleTooLong(index));
        }
        if item.status == PlanItemStatus::InProgress {
            in_progress_count += 1;
        }
        if let Some(item_id) = item.item_id.as_deref() {
            validate_item_id(item_id)?;
            if !explicit_ids.insert(item_id.to_string()) {
                return Err(PlanUpdateError::DuplicateItemId(item_id.to_string()));
            }
        }
    }
    if in_progress_count > 1 {
        return Err(PlanUpdateError::MultipleInProgress);
    }
    if creating
        && items.iter().any(|item| {
            matches!(
                item.status,
                PlanItemStatus::Completed | PlanItemStatus::Blocked | PlanItemStatus::Canceled
            )
        })
    {
        return Err(PlanUpdateError::NewPlanHasTerminalItem);
    }
    let all_terminal = items.iter().all(|item| {
        matches!(
            item.status,
            PlanItemStatus::Completed | PlanItemStatus::Canceled
        )
    });
    let has_blocked = items
        .iter()
        .any(|item| item.status == PlanItemStatus::Blocked);
    if in_progress_count == 0 && !all_terminal && !has_blocked {
        return Err(PlanUpdateError::MissingInProgress);
    }
    Ok(())
}

fn validate_plan_identity(
    current: Option<&SessionPlan>,
    input: &UpdatePlanInput,
) -> Result<(), PlanUpdateError> {
    let Some(current) = current else {
        if input.plan_id.is_some() {
            return Err(PlanUpdateError::UnexpectedPlanId);
        }
        if input.expected_revision != Some(0) {
            return Err(PlanUpdateError::RevisionConflict {
                expected: input.expected_revision.unwrap_or_default(),
                actual: 0,
            });
        }
        return Ok(());
    };
    let Some(plan_id) = input.plan_id.as_deref() else {
        return Err(PlanUpdateError::MissingPlanId);
    };
    if plan_id != current.plan_id.as_str() {
        return Err(PlanUpdateError::PlanIdMismatch);
    }
    if input.expected_revision != Some(current.revision) {
        return Err(PlanUpdateError::RevisionConflict {
            expected: input.expected_revision.unwrap_or_default(),
            actual: current.revision,
        });
    }
    if input.language != current.language {
        return Err(PlanUpdateError::LanguageChanged {
            expected: current.language.clone(),
            actual: input.language.clone(),
        });
    }
    Ok(())
}

fn normalize_plan_items(
    current: Option<&SessionPlan>,
    plan_id: &PlanId,
    inputs: Vec<UpdatePlanItemInput>,
) -> Result<Vec<PlanItem>, PlanUpdateError> {
    let mut generated_index = 1usize;
    inputs
        .into_iter()
        .map(|input| {
            let item_id = match input.item_id {
                Some(item_id) => PlanItemId::new(item_id),
                None if current.is_none() => {
                    let item_id = PlanItemId::new(format!("{}-item-{}", plan_id, generated_index));
                    generated_index += 1;
                    item_id
                }
                None => return Err(PlanUpdateError::MissingItemId),
            };
            Ok(PlanItem::new(item_id, input.step.trim(), input.status))
        })
        .collect()
}

fn validate_status_transitions(
    current: Option<&SessionPlan>,
    next: &[PlanItem],
) -> Result<(), PlanUpdateError> {
    let Some(current) = current else {
        return Ok(());
    };
    for next_item in next {
        if let Some(current_item) = current
            .items
            .iter()
            .find(|item| item.item_id == next_item.item_id)
        {
            if !current_item.status.can_transition_to(next_item.status) {
                return Err(PlanUpdateError::InvalidTransition {
                    item_id: next_item.item_id.to_string(),
                    from: current_item.status.as_str().to_string(),
                    to: next_item.status.as_str().to_string(),
                });
            }
        } else if next_item.status != PlanItemStatus::Pending {
            return Err(PlanUpdateError::NewItemMustBePending(
                next_item.item_id.to_string(),
            ));
        }
    }
    for current_item in &current.items {
        if !next.iter().any(|item| item.item_id == current_item.item_id) {
            if matches!(
                current_item.status,
                PlanItemStatus::InProgress | PlanItemStatus::Completed
            ) {
                return Err(PlanUpdateError::CannotRemoveActiveItem(
                    current_item.item_id.to_string(),
                ));
            }
            if current.task_bindings.iter().any(|(task_id, item_id)| {
                item_id == &current_item.item_id
                    && current.task_statuses.get(task_id).is_none_or(|status| {
                        matches!(status, TaskStatus::Pending | TaskStatus::Running)
                    })
            }) {
                return Err(PlanUpdateError::CannotRemoveBoundItem(
                    current_item.item_id.to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_item_id(item_id: &str) -> Result<(), PlanUpdateError> {
    let value = item_id.trim();
    if value.is_empty()
        || value.len() > 80
        || !value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '_' | '-')
        })
    {
        return Err(PlanUpdateError::InvalidItemId(item_id.to_string()));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanUpdateError {
    #[error("update_plan arguments 不是合法 JSON: {0}")]
    InvalidJson(String),
    #[error("计划语言标识非法: {0}")]
    InvalidLanguage(String),
    #[error("计划不能为空")]
    EmptyPlan,
    #[error("计划步骤过多: {0}")]
    TooManyItems(usize),
    #[error("计划步骤 {0} 标题不能为空")]
    BlankTitle(usize),
    #[error("计划步骤 {0} 标题过长")]
    TitleTooLong(usize),
    #[error("计划同时只能有一个步骤处于 in_progress")]
    MultipleInProgress,
    #[error("非终态计划必须有且仅有一个 in_progress 步骤")]
    MissingInProgress,
    #[error("计划项 ID 非法: {0}")]
    InvalidItemId(String),
    #[error("计划项 ID 重复: {0}")]
    DuplicateItemId(String),
    #[error("首次创建计划时不能传 planId，计划 ID 由后端生成")]
    UnexpectedPlanId,
    #[error("更新现有计划时每个步骤都必须携带 itemId")]
    MissingItemId,
    #[error("更新现有计划时必须携带 planId")]
    MissingPlanId,
    #[error("计划 ID 与当前计划不一致")]
    PlanIdMismatch,
    #[error("计划版本冲突：期望 {expected}，当前 {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("计划语言不能在更新过程中切换：期望 {expected}，实际 {actual}")]
    LanguageChanged { expected: String, actual: String },
    #[error("新计划只能从 pending 和一个 in_progress 步骤开始")]
    NewPlanHasTerminalItem,
    #[error("新计划项必须从 pending 开始: {0}")]
    NewItemMustBePending(String),
    #[error("计划项状态转换非法: {item_id} {from} -> {to}")]
    InvalidTransition {
        item_id: String,
        from: String,
        to: String,
    },
    #[error("不能从计划中移除进行中或已完成步骤: {0}")]
    CannotRemoveActiveItem(String),
    #[error("当前会话没有计划")]
    MissingPlan,
    #[error("计划项不存在: {0}")]
    UnknownItem(String),
    #[error("代理只能绑定当前 in_progress 计划项: {0}")]
    ItemNotActive(String),
    #[error("不能移除仍绑定活跃任务的计划项: {0}")]
    CannotRemoveBoundItem(String),
    #[error("计划存储失败: {0}")]
    Store(String),
}

pub fn execute_update_plan_tool(
    event_bus: &InMemoryEventBus,
    plan_store: &PlanStore,
    session_id: &SessionId,
    workspace_id: Option<&WorkspaceId>,
    task_id: Option<&TaskId>,
    mission_id: Option<&magi_core::MissionId>,
    arguments: &str,
) -> (String, magi_core::ExecutionResultStatus) {
    use magi_core::ExecutionResultStatus;
    debug_assert_eq!(plan_store.session_id, *session_id);
    match parse_update_plan_arguments(arguments).and_then(|input| plan_store.update(input)) {
        Ok(plan) => {
            publish_plan_event(
                event_bus,
                plan_event_type(&plan),
                &plan,
                workspace_id,
                task_id,
                mission_id,
            );
            (
                serde_json::json!({
                    "tool": "update_plan",
                    "status": "succeeded",
                    "plan": plan,
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }
        Err(error) => (
            serde_json::json!({
                "tool": "update_plan",
                "status": "failed",
                "error": error.to_string(),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        ),
    }
}

pub fn plan_event_type(plan: &SessionPlan) -> &'static str {
    match plan.state {
        PlanState::Paused => "session.plan.paused",
        PlanState::Completed | PlanState::Canceled => "session.plan.completed",
        PlanState::Active => "session.plan.updated",
    }
}

pub fn execute_session_update_plan_tool(
    event_bus: &InMemoryEventBus,
    plan_store: &PlanStore,
    session_id: &SessionId,
    workspace_id: Option<&WorkspaceId>,
    mission_id: &magi_core::MissionId,
    arguments: &str,
) -> (String, magi_core::ExecutionResultStatus) {
    execute_update_plan_tool(
        event_bus,
        plan_store,
        session_id,
        workspace_id,
        None,
        Some(mission_id),
        arguments,
    )
}

pub fn publish_plan_event(
    event_bus: &InMemoryEventBus,
    event_type: &str,
    plan: &SessionPlan,
    workspace_id: Option<&WorkspaceId>,
    task_id: Option<&TaskId>,
    mission_id: Option<&magi_core::MissionId>,
) {
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-plan-{}-{}", plan.plan_id, plan.revision)),
            event_type,
            serde_json::json!({
                "session_id": plan.session_id.to_string(),
                "workspace_id": workspace_id.map(ToString::to_string),
                "plan": plan,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: Some(plan.session_id.clone()),
            mission_id: mission_id.cloned(),
            task_id: task_id.cloned(),
            ..EventContext::default()
        }),
    );
}

pub fn publish_plan_cleared_event(
    event_bus: &InMemoryEventBus,
    plan: &SessionPlan,
    workspace_id: Option<&WorkspaceId>,
) {
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "event-plan-cleared-{}-{}",
                plan.plan_id,
                UtcMillis::now().0
            )),
            "session.plan.cleared",
            serde_json::json!({
                "session_id": plan.session_id.to_string(),
                "workspace_id": workspace_id.map(ToString::to_string),
                "plan_id": plan.plan_id.to_string(),
                "revision": plan.revision,
                "plan": null,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: Some(plan.session_id.clone()),
            ..EventContext::default()
        }),
    );
}

pub fn default_language() -> &'static str {
    DEFAULT_PLAN_LANGUAGE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store(name: &str) -> PlanStore {
        let session_store = Arc::new(SessionStore::new());
        let session_id = SessionId::new(name);
        session_store
            .create_session(session_id.clone(), name)
            .expect("session should create");
        PlanStore::new(session_store, session_id)
    }

    #[test]
    fn creates_plan_with_stable_generated_ids() {
        let store = test_store("generated-ids");
        let plan = store
            .update(UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![UpdatePlanItemInput {
                    item_id: None,
                    step: "检查执行链".to_string(),
                    status: PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should create");
        assert_eq!(plan.revision, 1);
        assert!(!plan.items[0].item_id.as_str().is_empty());
    }

    #[test]
    fn rejects_pending_to_completed_transition() {
        let store = test_store("transition");
        let created = store
            .update(UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                language: "en-US".to_string(),
                explanation: None,
                plan: vec![
                    UpdatePlanItemInput {
                        item_id: Some("execute".to_string()),
                        step: "Execute runtime".to_string(),
                        status: PlanItemStatus::InProgress,
                    },
                    UpdatePlanItemInput {
                        item_id: Some("inspect".to_string()),
                        step: "Inspect runtime".to_string(),
                        status: PlanItemStatus::Pending,
                    },
                ],
            })
            .expect("plan should create");
        let error = store
            .update(UpdatePlanInput {
                plan_id: Some(created.plan_id.to_string()),
                expected_revision: Some(created.revision),
                language: "en-US".to_string(),
                explanation: None,
                plan: vec![
                    UpdatePlanItemInput {
                        item_id: Some("execute".to_string()),
                        step: "Execute runtime".to_string(),
                        status: PlanItemStatus::InProgress,
                    },
                    UpdatePlanItemInput {
                        item_id: Some("inspect".to_string()),
                        step: "Inspect runtime".to_string(),
                        status: PlanItemStatus::Completed,
                    },
                ],
            })
            .expect_err("pending cannot jump to completed");
        assert!(matches!(error, PlanUpdateError::InvalidTransition { .. }));
    }

    #[test]
    fn rejects_stale_revision() {
        let store = test_store("revision");
        let created = store
            .update(UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![UpdatePlanItemInput {
                    item_id: Some("inspect".to_string()),
                    step: "检查现状".to_string(),
                    status: PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should create");
        let error = store
            .update(UpdatePlanInput {
                plan_id: Some(created.plan_id.to_string()),
                expected_revision: Some(0),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![UpdatePlanItemInput {
                    item_id: Some("inspect".to_string()),
                    step: "检查现状".to_string(),
                    status: PlanItemStatus::InProgress,
                }],
            })
            .expect_err("stale revision should fail");
        assert!(matches!(error, PlanUpdateError::RevisionConflict { .. }));
    }

    #[test]
    fn rejects_existing_plan_update_without_plan_id() {
        let store = test_store("missing-plan-id");
        let created = store
            .update(UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![UpdatePlanItemInput {
                    item_id: Some("inspect".to_string()),
                    step: "检查现状".to_string(),
                    status: PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should create");
        let error = store
            .update(UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(created.revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![UpdatePlanItemInput {
                    item_id: Some("inspect".to_string()),
                    step: "检查现状".to_string(),
                    status: PlanItemStatus::Completed,
                }],
            })
            .expect_err("existing plan update must include plan id");
        assert_eq!(error, PlanUpdateError::MissingPlanId);
    }

    #[test]
    fn rejects_active_plan_without_in_progress_item() {
        let store = test_store("missing-in-progress");
        let error = store
            .update(UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![UpdatePlanItemInput {
                    item_id: Some("inspect".to_string()),
                    step: "检查现状".to_string(),
                    status: PlanItemStatus::Pending,
                }],
            })
            .expect_err("active plan must expose one in-progress item");
        assert_eq!(error, PlanUpdateError::MissingInProgress);
    }

    #[test]
    fn pause_and_resume_preserve_active_item() {
        let store = test_store("pause-resume");
        let created = store
            .update(UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![UpdatePlanItemInput {
                    item_id: Some("inspect".to_string()),
                    step: "检查现状".to_string(),
                    status: PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should create");
        let paused = store
            .pause()
            .expect("pause should succeed")
            .expect("plan should exist");
        assert_eq!(paused.state, PlanState::Paused);
        assert!(paused.revision > created.revision);
        let resumed = store
            .resume()
            .expect("resume should succeed")
            .expect("plan should exist");
        assert_eq!(resumed.state, PlanState::Active);
        assert_eq!(resumed.items[0].status, PlanItemStatus::InProgress);
    }

    #[test]
    fn bound_task_completion_advances_next_plan_item() {
        let store = test_store("task-advance");
        let created = store
            .update(UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![
                    UpdatePlanItemInput {
                        item_id: Some("implement".to_string()),
                        step: "完成实现".to_string(),
                        status: PlanItemStatus::InProgress,
                    },
                    UpdatePlanItemInput {
                        item_id: Some("verify".to_string()),
                        step: "验证结果".to_string(),
                        status: PlanItemStatus::Pending,
                    },
                ],
            })
            .expect("plan should create");
        let task_id = TaskId::new("task-implement");
        store
            .bind_task(task_id.clone(), created.items[0].item_id.clone())
            .expect("active item should accept task binding");
        let updated = store
            .sync_task_status(&task_id, TaskStatus::Completed)
            .expect("task status should sync")
            .expect("plan should exist");
        assert_eq!(updated.items[0].status, PlanItemStatus::Completed);
        assert_eq!(updated.items[1].status, PlanItemStatus::InProgress);
        assert_eq!(updated.state, PlanState::Active);
    }

    #[test]
    fn task_cannot_bind_pending_plan_item() {
        let store = test_store("pending-binding");
        let created = store
            .update(UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![
                    UpdatePlanItemInput {
                        item_id: Some("implement".to_string()),
                        step: "完成实现".to_string(),
                        status: PlanItemStatus::InProgress,
                    },
                    UpdatePlanItemInput {
                        item_id: Some("verify".to_string()),
                        step: "验证结果".to_string(),
                        status: PlanItemStatus::Pending,
                    },
                ],
            })
            .expect("plan should create");
        let error = store
            .bind_task(TaskId::new("task-verify"), created.items[1].item_id.clone())
            .expect_err("pending item must reject task binding");
        assert!(matches!(error, PlanUpdateError::ItemNotActive(_)));
    }

    #[test]
    fn update_tool_publishes_completed_event() {
        let store = test_store("completed-event");
        let event_bus = InMemoryEventBus::new(16);
        let session_id = SessionId::new("completed-event");
        let (created_payload, created_status) = execute_update_plan_tool(
            &event_bus,
            &store,
            &session_id,
            None,
            None,
            None,
            &serde_json::json!({
                "expectedRevision": 0,
                "language": "zh-CN",
                "plan": [{
                    "itemId": "implement",
                    "step": "完成实现",
                    "status": "in_progress"
                }]
            })
            .to_string(),
        );
        assert_eq!(created_status, magi_core::ExecutionResultStatus::Succeeded);
        let created: serde_json::Value =
            serde_json::from_str(&created_payload).expect("tool payload should be json");
        let plan_id = created["plan"]["planId"]
            .as_str()
            .expect("planId should exist");
        let revision = created["plan"]["revision"]
            .as_u64()
            .expect("revision should exist");
        let (_, completed_status) = execute_update_plan_tool(
            &event_bus,
            &store,
            &session_id,
            None,
            None,
            None,
            &serde_json::json!({
                "planId": plan_id,
                "expectedRevision": revision,
                "language": "zh-CN",
                "plan": [{
                    "itemId": "implement",
                    "step": "完成实现",
                    "status": "completed"
                }]
            })
            .to_string(),
        );
        assert_eq!(
            completed_status,
            magi_core::ExecutionResultStatus::Succeeded
        );
        assert!(event_bus.snapshot().recent_events.iter().any(|event| {
            event.event_type == "session.plan.completed"
                && event.payload["plan"]["state"].as_str() == Some("completed")
        }));
    }
}
