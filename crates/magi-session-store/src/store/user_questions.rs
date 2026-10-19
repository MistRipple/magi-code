//! 人工交互沿原工具 item 提交，答复落盘成功后运行时才能唤醒等待方。
use super::{CanonicalTurnMutation, SessionStore};
use crate::user_question::{
    PendingUserQuestion, USER_QUESTION_METADATA_KEY, UserQuestionResponse, UserQuestionState,
    validate_answers,
};
use crate::{CanonicalTurn, CanonicalTurnItem, CanonicalTurnStatus, SessionStoreState};
use magi_core::{DomainError, DomainResult, SessionId, UtcMillis};

fn invalid(message: impl Into<String>) -> DomainError {
    DomainError::InvalidState {
        message: message.into(),
    }
}

fn accepts_response(
    store: &SessionStore,
    state: &SessionStoreState,
    turn: &CanonicalTurn,
    item: &CanonicalTurnItem,
    request: &PendingUserQuestion,
) -> bool {
    if !super::agent_turns::interaction_turn_is_current_or_recoverable(store, state, turn)
        .unwrap_or(false)
    {
        return false;
    }
    if !turn.status.is_terminal() && turn.completed_at.is_none() {
        return !item.status.is_terminal();
    }
    // 本地任务在重启后由 Continue 接管。网页问题仍需由原网页调用方存活保证。
    request.task_id.is_some()
        && item
            .tool
            .as_ref()
            .and_then(|tool| tool.result.as_ref())
            .and_then(|result| result.get("status"))
            .and_then(serde_json::Value::as_str)
            == Some("awaiting_user_input")
        && turn.status == CanonicalTurnStatus::Interrupted
        && (turn.agent_turn_binding().ok().flatten().is_some()
            || turn.items.iter().any(|item| {
                item.metadata
                    .get("noticeKind")
                    .and_then(serde_json::Value::as_str)
                    == Some("session_interrupted")
                    && item
                        .metadata
                        .get("recoveryState")
                        .and_then(serde_json::Value::as_str)
                        == Some("ready")
            }))
}

impl CanonicalTurnItem {
    pub fn user_question(&self) -> DomainResult<Option<UserQuestionState>> {
        let question: Option<UserQuestionState> = self
            .metadata
            .get(USER_QUESTION_METADATA_KEY)
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .map_err(|error| invalid(format!("提问持久状态无效：{error}")))?;
        if let Some(question) = &question {
            let request = &question.request;
            if request.question_id.trim().is_empty()
                || request.session_id != self.session_id
                || request.turn_id != self.turn_id
                || request.task_id.as_ref()
                    != self
                        .worker
                        .as_ref()
                        .and_then(|worker| worker.task_id.as_ref())
                || self.tool.as_ref().is_none_or(|tool| {
                    tool.name != "ask_user_question" || tool.call_id != request.tool_call_id
                })
                || request.questions.is_empty()
            {
                return Err(invalid("提问与工具 item 来源不匹配"));
            }
            if question.cancelled_at.is_some() && question.response.is_some() {
                return Err(invalid("已取消问题不能保留答复"));
            }
            if let Some(response) = &question.response
                && validate_response(request, response.clone())? != *response
            {
                return Err(invalid("持久答复不是规范化结果"));
            }
        }
        Ok(question)
    }
}

fn validate_response(
    request: &PendingUserQuestion,
    response: UserQuestionResponse,
) -> DomainResult<UserQuestionResponse> {
    match response {
        UserQuestionResponse::Answered { answers } => Ok(UserQuestionResponse::Answered {
            answers: validate_answers(&request.questions, &answers).map_err(invalid)?,
        }),
        UserQuestionResponse::Skipped if request.can_skip => Ok(UserQuestionResponse::Skipped),
        UserQuestionResponse::Skipped => Err(invalid("这个问题不支持跳过")),
    }
}

impl SessionStore {
    pub fn pending_user_questions(
        &self,
        session: &SessionId,
    ) -> DomainResult<Vec<PendingUserQuestion>> {
        let state = self.state.read().expect("session state lock");
        pending_user_questions_in_state(self, &state, session)
    }

    pub fn user_question_state(
        &self,
        session: &SessionId,
        id: &str,
    ) -> DomainResult<Option<UserQuestionState>> {
        let state = self.state.read().expect("session state lock");
        for item in state
            .canonical_turns
            .iter()
            .filter(|turn| &turn.session_id == session)
            .flat_map(|turn| &turn.items)
        {
            if let Some(question) = item.user_question()?
                && question.request.question_id == id
            {
                return Ok(Some(question));
            }
        }
        Ok(None)
    }

    pub fn register_user_question(&self, request: PendingUserQuestion) -> DomainResult<()> {
        let session = request.session_id.clone();
        self.commit_user_question(&session, |state| {
            let previous = state
                .canonical_turns
                .iter()
                .find(|turn| turn.session_id == session && turn.turn_id == request.turn_id)
                .ok_or_else(|| invalid("提问来源 Turn 不存在"))?;
            if previous.status.is_terminal() {
                return Err(invalid("已结束轮次不能发起提问"));
            }
            let mut next = previous.clone();
            let item = next
                .items
                .iter_mut()
                .rev()
                .find(|item| {
                    item.worker
                        .as_ref()
                        .and_then(|worker| worker.task_id.as_ref())
                        == request.task_id.as_ref()
                        && item.tool.as_ref().is_some_and(|tool| {
                            tool.name == "ask_user_question" && tool.call_id == request.tool_call_id
                        })
                })
                .ok_or_else(|| invalid("提问缺少 canonical 工具 item"))?;
            if item.user_question()?.is_some() {
                return Err(invalid("该工具调用已经发起提问"));
            }
            if !accepts_response(self, state, previous, item, &request) {
                return Err(invalid("提问目标已关闭或工具调用已结束"));
            }
            item.tool.as_mut().expect("question tool").result = Some(serde_json::json!({
                "tool": "ask_user_question", "status": "awaiting_user_input",
                "question_id": request.question_id, "questions": request.questions,
            }));
            let id = item.item_id.clone();
            item.metadata.insert(
                USER_QUESTION_METADATA_KEY.into(),
                serde_json::to_value(UserQuestionState {
                    request: request.clone(),
                    response: None,
                    cancelled_at: None,
                })
                .expect("question serializable"),
            );
            item.user_question()?;
            advance_item_version(item)?;
            next.validate_update_from(previous)?;
            Ok((
                CanonicalTurnMutation {
                    previous: Some(previous.clone()),
                    next,
                },
                id,
            ))
        })
        .map(|_| ())
    }

    pub fn resolve_user_question(
        &self,
        session: &SessionId,
        id: &str,
        response: UserQuestionResponse,
    ) -> DomainResult<UserQuestionState> {
        self.commit_user_question(session, |state| {
            for previous in state
                .canonical_turns
                .iter()
                .filter(|turn| &turn.session_id == session)
            {
                for (index, item) in previous.items.iter().enumerate() {
                    let Some(mut question) = item.user_question()? else {
                        continue;
                    };
                    if question.request.question_id != id {
                        continue;
                    }
                    if question.response.is_some()
                        || question.cancelled_at.is_some()
                        || !accepts_response(self, state, previous, item, &question.request)
                    {
                        return Err(invalid("这个问题已经处理过，或所在的对话轮次已经结束"));
                    }
                    question.response = Some(validate_response(&question.request, response)?);
                    let mut next = previous.clone();
                    let item = &mut next.items[index];
                    item.tool.as_mut().expect("question tool").result = question.result_payload();
                    item.metadata.insert(
                        USER_QUESTION_METADATA_KEY.into(),
                        serde_json::to_value(&question).expect("question serializable"),
                    );
                    advance_item_version(item)?;
                    next.validate_update_from(previous)?;
                    return Ok((
                        CanonicalTurnMutation {
                            previous: Some(previous.clone()),
                            next,
                        },
                        item_id(previous, index),
                    ));
                }
            }
            Err(invalid("这个问题已经处理过，或所在的对话轮次已经结束"))
        })?
        .user_question()?
        .ok_or_else(|| invalid("提问提交后丢失"))
    }

    pub fn cancel_user_question(&self, session: &SessionId, id: &str) -> DomainResult<()> {
        self.commit_user_question(session, |state| {
            for previous in state
                .canonical_turns
                .iter()
                .filter(|turn| &turn.session_id == session)
            {
                for (index, item) in previous.items.iter().enumerate() {
                    let Some(mut question) = item.user_question()? else {
                        continue;
                    };
                    if question.request.question_id != id {
                        continue;
                    }
                    let mut next = previous.clone();
                    if question.response.is_none() && question.cancelled_at.is_none() {
                        question.cancelled_at = Some(UtcMillis::now());
                        let item = &mut next.items[index];
                        item.metadata.insert(
                            USER_QUESTION_METADATA_KEY.into(),
                            serde_json::to_value(question).expect("question serializable"),
                        );
                        advance_item_version(item)?;
                    }
                    next.validate_update_from(previous)?;
                    return Ok((
                        CanonicalTurnMutation {
                            previous: Some(previous.clone()),
                            next,
                        },
                        item_id(previous, index),
                    ));
                }
            }
            Err(invalid("提问不存在"))
        })
        .map(|_| ())
    }

    fn commit_user_question(
        &self,
        session: &SessionId,
        prepare: impl FnOnce(&SessionStoreState) -> DomainResult<(CanonicalTurnMutation, String)>,
    ) -> DomainResult<CanonicalTurnItem> {
        self.commit_tool_interaction(session, |state| {
            let (mutation, id) = prepare(state)?;
            let item = mutation
                .next
                .items
                .iter()
                .find(|item| item.item_id == id)
                .expect("question item")
                .clone();
            Ok((vec![mutation], item))
        })
    }
}

fn item_id(turn: &CanonicalTurn, index: usize) -> String {
    turn.items[index].item_id.clone()
}
pub(super) fn advance_item_version(item: &mut CanonicalTurnItem) -> DomainResult<()> {
    item.item_version = Some(
        item.item_version
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| invalid("item version 溢出"))?,
    );
    item.updated_at = UtcMillis(UtcMillis::now().0.max(item.updated_at.0.saturating_add(1)));
    Ok(())
}

pub(super) fn pending_user_questions_in_state(
    store: &SessionStore,
    state: &SessionStoreState,
    session: &SessionId,
) -> DomainResult<Vec<PendingUserQuestion>> {
    let mut pending = Vec::new();
    for turn in state
        .canonical_turns
        .iter()
        .filter(|turn| &turn.session_id == session)
    {
        for item in &turn.items {
            if let Some(question) = item.user_question()?
                && question.response.is_none()
                && question.cancelled_at.is_none()
                && accepts_response(store, state, turn, item, &question.request)
            {
                pending.push(question.request);
            }
        }
    }
    pending.sort_by(|a, b| {
        a.requested_at
            .cmp(&b.requested_at)
            .then(a.question_id.cmp(&b.question_id))
    });
    Ok(pending)
}
