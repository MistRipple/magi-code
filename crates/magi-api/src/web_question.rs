//! 网页原生问题与 Magi 现有用户问题注册表的桥接。页面拥有步骤，不伪造模型工具调用。
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use magi_conversation_runtime::session_writeback::{
    ExternalToolItemWriter, TOOL_ORIGIN_CHATGPT_WEB,
};
use magi_conversation_runtime::{
    PendingUserQuestion, UserQuestion, UserQuestionOption, UserQuestionResponse,
};
use magi_core::{EventId, SessionId, TaskStatus, UtcMillis};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_web_model::{
    DriverFuture, WebModelError, WebModelErrorCode, WebModelIdentity, WebQuestion,
    WebQuestionAnswer, WebQuestionSelection, WebQuestionSink,
};

use crate::state::ApiState;

struct ApiWebQuestionSink {
    state: ApiState,
}

impl ApiState {
    pub fn web_question_sink(&self) -> Arc<dyn WebQuestionSink> {
        Arc::new(ApiWebQuestionSink {
            state: self.clone(),
        })
    }
}

fn map_answer(
    question: &WebQuestion,
    response: UserQuestionResponse,
) -> Result<WebQuestionAnswer, WebModelError> {
    match response {
        UserQuestionResponse::Skipped if question.can_skip => Ok(WebQuestionAnswer::Skipped),
        UserQuestionResponse::Answered { answers } if answers.len() == question.questions.len() => {
            let selections = question
                .questions
                .iter()
                .zip(answers)
                .map(|(item, answer)| {
                    if answer.other.is_some() && !item.allows_text {
                        return Err(invalid("网页卡片没有自定义输入框"));
                    }
                    let selected = answer
                        .selected
                        .iter()
                        .map(|label| {
                            item.options
                                .iter()
                                .position(|option| &option.label == label)
                                .ok_or_else(|| invalid("网页问题的选项已经变化"))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(WebQuestionSelection {
                        selected,
                        text: answer.other,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(WebQuestionAnswer::Answered {
                answers: selections,
            })
        }
        _ => Err(invalid("回答与网页卡片不匹配")),
    }
}

fn invalid(message: &str) -> WebModelError {
    WebModelError::new(WebModelErrorCode::WebSelectorsDrift, message)
}

fn wait_for_answer(
    state: ApiState,
    session_id: SessionId,
    question: WebQuestion,
    cancelled: Arc<AtomicBool>,
) -> Result<WebQuestionAnswer, WebModelError> {
    let turn_id = state
        .session_store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn.map(|turn| turn.turn_id))
        .ok_or_else(WebModelError::cancelled)?;
    let active = || {
        !cancelled.load(Ordering::Acquire)
            && state
                .session_store
                .turn_execution_is_active(&session_id, &turn_id)
    };
    if !active() {
        return Err(WebModelError::cancelled());
    }
    let workspace_id = state
        .session_store
        .execution_ownership(&session_id)
        .and_then(|owner| owner.workspace_id);
    let call_id = format!("web-question-{}-{}", turn_id, question.id);
    let questions: Vec<UserQuestion> = question
        .questions
        .iter()
        .map(|item| UserQuestion {
            question: item.question.clone(),
            header: "GPT Web".to_string(),
            multi_select: item.multi_select,
            allow_other: item.allows_text,
            options: item
                .options
                .iter()
                .map(|option| UserQuestionOption {
                    label: option.label.clone(),
                    description: option.description.clone(),
                })
                .collect(),
        })
        .collect();
    let arguments = serde_json::json!({ "questions": questions }).to_string();
    let writer = ExternalToolItemWriter {
        session_store: &state.session_store,
        event_bus: &state.event_bus,
        session_id: &session_id,
        workspace_id: &workspace_id,
        turn_id: Some(&turn_id),
        origin: Some(TOOL_ORIGIN_CHATGPT_WEB),
    };
    writer
        .started(&call_id, "ask_user_question", &arguments)
        .map_err(|error| invalid(&error))?;
    let request = PendingUserQuestion {
        question_id: call_id.clone(),
        session_id: session_id.clone(),
        task_id: None,
        turn_id: turn_id.clone(),
        tool_call_id: call_id.clone(),
        questions: questions.clone(),
        requested_at: UtcMillis::now(),
        can_skip: question.can_skip,
    };
    let registry = state.conversation_registry.user_questions();
    let waiter = registry
        .request_persisted(&state.session_store, request)
        .map_err(|error| invalid(&error))?;
    let publish = |name: &str, outcome: Option<&str>| {
        let _ = state.event_bus.publish(
            EventEnvelope::domain(
                EventId::unique(name),
                name,
                serde_json::json!({
                    "session_id": session_id, "workspace_id": workspace_id, "turn_id": turn_id,
                    "question_id": call_id, "tool_call_id": call_id, "outcome": outcome,
                }),
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                workspace_id: workspace_id.clone(),
                ..Default::default()
            }),
        );
    };
    publish("user.question.requested", None);
    // 有其它任务仍在使用工作区时不得释放其租约。
    let other_tasks = state
        .session_store
        .execution_task_ids_for_session(&session_id)
        .iter()
        .any(|id| {
            state
                .task_store()
                .and_then(|store| store.get_task(id))
                .is_some_and(|task| {
                    matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                })
        });
    let human_wait = state
        .conversation_registry
        .human_wait()
        .begin(&session_id, !other_tasks);
    let response = loop {
        if !active() {
            break Err(WebModelError::cancelled());
        }
        match waiter.response_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(response) => {
                if !active() || !human_wait.finish(active) {
                    break Err(WebModelError::cancelled());
                }
                break Ok(response);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break Err(WebModelError::cancelled());
            }
        }
    };
    registry.cancel(&call_id);
    let (payload, status) = match &response {
        Ok(UserQuestionResponse::Answered { answers }) => (
            magi_conversation_runtime::user_question::answered_result_payload(&questions, answers),
            magi_core::ExecutionResultStatus::Succeeded,
        ),
        Ok(UserQuestionResponse::Skipped) => (
            serde_json::json!({ "status": "skipped" }),
            magi_core::ExecutionResultStatus::Succeeded,
        ),
        Err(_) => {
            publish("user.question.resolved", Some("cancelled"));
            (
                serde_json::json!({ "status": "cancelled" }),
                magi_core::ExecutionResultStatus::Cancelled,
            )
        }
    };
    if let Err(error) = writer.finished(
        &call_id,
        "ask_user_question",
        &arguments,
        &payload.to_string(),
        status,
    ) {
        if response.is_ok() {
            return Err(invalid(&error));
        }
        tracing::warn!(%error, "网页问题取消后的历史项收口失败");
    }
    map_answer(&question, response?)
}

impl WebQuestionSink for ApiWebQuestionSink {
    fn ask<'a>(
        &'a self,
        identity: &'a WebModelIdentity,
        question: &'a WebQuestion,
        cancelled: Arc<AtomicBool>,
    ) -> DriverFuture<'a, WebQuestionAnswer> {
        let state = self.state.clone();
        let session_id = SessionId::new(&identity.session_id);
        let question = question.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                wait_for_answer(state, session_id, question, cancelled)
            })
            .await
            .map_err(|error| invalid(&format!("网页问题等待失败：{error}")))?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_conversation_runtime::UserQuestionAnswer;
    use magi_web_model::{WebQuestionItem, WebQuestionOption};

    fn card() -> WebQuestion {
        WebQuestion {
            id: "card:1".into(),
            can_skip: false,
            questions: vec![WebQuestionItem {
                question: "主食？".into(),
                multi_select: false,
                allows_text: false,
                options: ["米饭", "面条", "米粉", "饺子", "炸物", "其他"]
                    .into_iter()
                    .map(|label| WebQuestionOption {
                        label: label.into(),
                        description: String::new(),
                    })
                    .collect(),
            }],
        }
    }

    fn state() -> ApiState {
        let state = ApiState::new(
            "question-test",
            Arc::new(magi_event_bus::InMemoryEventBus::new(64)),
            Arc::new(magi_session_store::SessionStore::default()),
            Arc::new(magi_workspace::WorkspaceStore::default()),
            Arc::new(magi_governance::GovernanceService::default()),
        );
        let session_id = SessionId::new("s1");
        state
            .session_store
            .create_session(session_id.clone(), "questions")
            .unwrap();
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &state.session_store,
            state.turn_coordinator(),
            &session_id,
            "turn1",
            1,
            UtcMillis::now(),
            "running",
            "晚餐",
        );
        state
    }

    async fn pending(state: &ApiState) -> PendingUserQuestion {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(question) = state
                    .conversation_registry
                    .user_questions()
                    .pending_for_session(&SessionId::new("s1"))
                    .into_iter()
                    .next()
                {
                    return question;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("question registered")
    }

    fn start(
        state: &ApiState,
        cancelled: Arc<AtomicBool>,
    ) -> tokio::task::JoinHandle<Result<WebQuestionAnswer, WebModelError>> {
        let sink = state.web_question_sink();
        tokio::spawn(async move {
            let identity = WebModelIdentity {
                session_id: "s1".into(),
                project_id: "p1".into(),
                thread_id: "thread1".into(),
                engine_id: "openai.chatgpt-web".into(),
            };
            sink.ask(&identity, &card(), cancelled).await
        })
    }

    #[tokio::test]
    async fn web_question_uses_existing_registry_and_records_the_answer() {
        let state = state();
        let task = start(&state, Arc::new(AtomicBool::new(false)));
        let pending = pending(&state).await;
        assert_eq!(pending.questions[0].options.len(), 6);
        assert!(!pending.questions[0].allow_other);
        assert!(!pending.can_skip);
        let registry = state.conversation_registry.user_questions();
        assert!(
            registry
                .resolve(
                    &pending.session_id,
                    &pending.question_id,
                    UserQuestionResponse::Skipped
                )
                .is_err()
        );
        assert!(
            registry
                .resolve(
                    &pending.session_id,
                    &pending.question_id,
                    UserQuestionResponse::Answered {
                        answers: vec![UserQuestionAnswer {
                            selected: vec![],
                            other: Some("其它输入".into())
                        }]
                    }
                )
                .is_err()
        );
        registry
            .resolve(
                &pending.session_id,
                &pending.question_id,
                UserQuestionResponse::Answered {
                    answers: vec![UserQuestionAnswer {
                        selected: vec!["面条".into()],
                        other: None,
                    }],
                },
            )
            .unwrap();
        assert_eq!(
            task.await.unwrap().unwrap(),
            WebQuestionAnswer::Answered {
                answers: vec![WebQuestionSelection {
                    selected: vec![1],
                    text: None
                }]
            }
        );
        assert!(registry.pending_for_session(&pending.session_id).is_empty());
        let events = state.event_bus.snapshot();
        assert!(
            events
                .recent_events
                .iter()
                .any(|e| e.event_type == "user.question.requested")
        );
        assert!(
            events
                .recent_events
                .iter()
                .any(|e| e.payload.to_string().contains("面条")
                    && e.payload.to_string().contains("tool_call_result"))
        );
    }

    #[tokio::test]
    async fn cancelling_a_web_question_removes_the_pending_panel() {
        let state = state();
        let cancelled = Arc::new(AtomicBool::new(false));
        let task = start(&state, cancelled.clone());
        let pending = pending(&state).await;
        cancelled.store(true, Ordering::Release);
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert!(
            state
                .conversation_registry
                .user_questions()
                .pending_for_session(&pending.session_id)
                .is_empty()
        );
        assert!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|e| e.event_type == "user.question.resolved"
                    && e.payload["outcome"] == "cancelled")
        );
    }
}
