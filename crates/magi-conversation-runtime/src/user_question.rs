//! 用户提问的进程内等待通道；问题和答复由 SessionStore 持久化。
#[cfg(test)]
use magi_core::UtcMillis;
use magi_core::{SessionId, TaskId};
pub use magi_session_store::user_question::{
    MAX_OPTIONS, MAX_QUESTIONS, MIN_OPTIONS, PendingUserQuestion, UserQuestion, UserQuestionAnswer,
    UserQuestionOption, UserQuestionResponse, answered_result_payload, parse_questions,
    validate_answers,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};

struct PendingEntry {
    store: Option<magi_session_store::SessionStore>,
    request: PendingUserQuestion,
    response_tx: mpsc::Sender<UserQuestionResponse>,
}

impl std::fmt::Debug for PendingEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingEntry")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

pub struct UserQuestionWaiter {
    pub request: PendingUserQuestion,
    pub response_rx: mpsc::Receiver<UserQuestionResponse>,
}

#[derive(Clone, Debug, Default)]
pub struct UserQuestionRegistry {
    pending: Arc<Mutex<HashMap<String, PendingEntry>>>,
}

impl UserQuestionRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, PendingEntry>> {
        self.pending.lock().expect("user question registry lock")
    }

    #[cfg(test)]
    fn request(&self, request: PendingUserQuestion) -> UserQuestionWaiter {
        Self::insert_pending(&mut self.lock(), request, None)
    }

    pub fn request_persisted(
        &self,
        store: &magi_session_store::SessionStore,
        request: PendingUserQuestion,
    ) -> Result<UserQuestionWaiter, String> {
        let mut pending = self.lock();
        store
            .register_user_question(request.clone())
            .map_err(|error| error.to_string())?;
        Ok(Self::insert_pending(
            &mut pending,
            request,
            Some(store.clone()),
        ))
    }

    fn insert_pending(
        pending: &mut HashMap<String, PendingEntry>,
        request: PendingUserQuestion,
        store: Option<magi_session_store::SessionStore>,
    ) -> UserQuestionWaiter {
        let (response_tx, response_rx) = mpsc::channel();
        pending.insert(
            request.question_id.clone(),
            PendingEntry {
                store,
                request: request.clone(),
                response_tx,
            },
        );
        UserQuestionWaiter {
            request,
            response_rx,
        }
    }

    pub fn pending_for_session(&self, session_id: &SessionId) -> Vec<PendingUserQuestion> {
        let mut pending: Vec<PendingUserQuestion> = self
            .lock()
            .values()
            .filter(|entry| &entry.request.session_id == session_id)
            .map(|entry| entry.request.clone())
            .collect();
        pending.sort_by(|left, right| {
            left.requested_at
                .cmp(&right.requested_at)
                .then_with(|| left.question_id.cmp(&right.question_id))
        });
        pending
    }

    /// 用户回答（或跳过）。回答必须与题目对得上；已经收口的问题返回错误。
    pub fn resolve(
        &self,
        session_id: &SessionId,
        question_id: &str,
        response: UserQuestionResponse,
    ) -> Result<PendingUserQuestion, String> {
        let mut pending = self.lock();
        let Some(entry) = pending.get(question_id) else {
            return Err("这个问题已经处理过，或所在的对话轮次已经结束".to_string());
        };
        if &entry.request.session_id != session_id {
            return Err("问题不属于当前会话".to_string());
        }
        if matches!(response, UserQuestionResponse::Skipped) && !entry.request.can_skip {
            return Err("这个问题不支持跳过".to_string());
        }
        let response = match response {
            UserQuestionResponse::Answered { answers } => UserQuestionResponse::Answered {
                answers: validate_answers(&entry.request.questions, &answers)?,
            },
            UserQuestionResponse::Skipped => UserQuestionResponse::Skipped,
        };
        if let Some(store) = &entry.store {
            store
                .resolve_user_question(session_id, question_id, response.clone())
                .map_err(|error| match error {
                    magi_core::DomainError::Persistence { .. } => {
                        format!("提问答复持久化失败：{error}")
                    }
                    error => error.to_string(),
                })?;
        }
        let entry = pending.remove(question_id).expect("entry checked above");
        // 答复已持久化，即使等待方刚退出，也保留给恢复链消费。
        let _ = entry.response_tx.send(response);
        Ok(entry.request)
    }

    pub fn cancel(&self, question_id: &str) {
        if let Some(entry) = self.lock().remove(question_id) {
            Self::persist_cancel(&entry);
        }
    }

    fn persist_cancel(entry: &PendingEntry) {
        if let Some(store) = &entry.store
            && let Err(error) =
                store.cancel_user_question(&entry.request.session_id, &entry.request.question_id)
        {
            tracing::error!(%error, question_id = %entry.request.question_id, "提问取消事实提交失败");
        }
    }

    fn remove_where(&self, matches: impl Fn(&PendingUserQuestion) -> bool) {
        self.lock().retain(|_, entry| {
            if matches(&entry.request) {
                Self::persist_cancel(entry);
                false
            } else {
                true
            }
        });
    }

    pub fn remove_turn(&self, session_id: &SessionId, turn_id: &str) {
        self.remove_where(|request| {
            &request.session_id == session_id && request.turn_id == turn_id
        });
    }

    pub fn remove_task(&self, session_id: &SessionId, task_id: &TaskId) {
        self.remove_where(|request| {
            &request.session_id == session_id && request.task_id.as_ref() == Some(task_id)
        });
    }

    /// Session 删除时清理所有进程内等待项，并先记录持久取消事实。
    /// 丢弃发送端会让等待方立即收到通道关闭，不再持有已删除会话。
    pub fn remove_session(&self, session_id: &SessionId) {
        self.remove_where(|request| &request.session_id == session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_arguments() -> String {
        serde_json::json!({
            "questions": [
                {
                    "question": "用哪种数据库？",
                    "header": "数据库",
                    "multiSelect": false,
                    "options": [
                        { "label": "SQLite（推荐）", "description": "零运维" },
                        { "label": "Postgres" }
                    ]
                },
                {
                    "question": "要覆盖哪些平台？",
                    "header": "平台",
                    "multiSelect": true,
                    "options": [
                        { "label": "macOS" },
                        { "label": "Windows" },
                        { "label": "Linux" }
                    ]
                }
            ]
        })
        .to_string()
    }

    fn pending(questions: Vec<UserQuestion>) -> PendingUserQuestion {
        PendingUserQuestion {
            question_id: "q1".to_string(),
            session_id: SessionId::new("s1"),
            task_id: Some(TaskId::new("t1")),
            turn_id: "turn1".to_string(),
            tool_call_id: "call1".to_string(),
            questions,
            requested_at: UtcMillis(1),
            can_skip: true,
        }
    }

    #[test]
    fn remove_session_clears_only_the_deleted_session_questions() {
        let registry = UserQuestionRegistry::default();
        let questions = parse_questions(&sample_arguments()).unwrap();
        let task_waiter = registry.request(pending(questions.clone()));
        let mut taskless = pending(questions.clone());
        taskless.question_id = "q-taskless".to_string();
        taskless.task_id = None;
        let taskless_waiter = registry.request(taskless);
        let mut other = pending(questions);
        other.question_id = "q2".to_string();
        other.session_id = SessionId::new("s2");
        let other_waiter = registry.request(other);

        registry.remove_session(&SessionId::new("s1"));
        assert!(
            registry
                .pending_for_session(&SessionId::new("s1"))
                .is_empty()
        );
        assert_eq!(registry.pending_for_session(&SessionId::new("s2")).len(), 1);
        for waiter in [task_waiter, taskless_waiter] {
            assert_eq!(
                waiter.response_rx.try_recv(),
                Err(mpsc::TryRecvError::Disconnected)
            );
        }
        assert_eq!(
            other_waiter.response_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );
        registry
            .resolve(&SessionId::new("s2"), "q2", UserQuestionResponse::Skipped)
            .unwrap();
        assert_eq!(
            other_waiter.response_rx.try_recv().unwrap(),
            UserQuestionResponse::Skipped
        );
    }

    #[test]
    fn a_custom_other_answer_is_reported_as_the_users_final_decision() {
        let questions = parse_questions(&sample_arguments()).unwrap();
        let answers = vec![
            UserQuestionAnswer {
                selected: vec![],
                other: Some("SQL Server".to_string()),
            },
            UserQuestionAnswer {
                selected: vec!["macOS".to_string()],
                other: Some("FreeBSD".to_string()),
            },
        ];
        let payload = answered_result_payload(&questions, &answers);
        assert_eq!(
            payload["answers"][0]["answer"],
            serde_json::json!(["SQL Server"])
        );
        assert_eq!(payload["answers"][0]["custom_answer"], true);
        assert_eq!(
            payload["answers"][1]["answer"],
            serde_json::json!(["macOS", "FreeBSD"])
        );
        let preset_only = answered_result_payload(
            &questions[..1],
            &[UserQuestionAnswer {
                selected: vec!["Postgres".to_string()],
                other: None,
            }],
        );
        assert_eq!(preset_only["answers"][0]["custom_answer"], false);
        assert!(
            payload["instruction"]
                .as_str()
                .unwrap()
                .contains("合法的决定"),
            "必须明确告诉模型“其他”是合法决定"
        );
    }

    #[test]
    fn parses_valid_arguments_and_rejects_bad_shapes() {
        let questions = parse_questions(&sample_arguments()).unwrap();
        assert_eq!(questions.len(), 2);
        assert!(questions[1].multi_select);

        assert!(parse_questions(r#"{"questions":[]}"#).is_err());
        let one_option = serde_json::json!({"questions":[{"question":"q","header":"h","multiSelect":false,"options":[{"label":"a"}]}]});
        assert!(
            parse_questions(&one_option.to_string())
                .unwrap_err()
                .contains("2–4")
        );
        let other = serde_json::json!({"questions":[{"question":"q","header":"h","multiSelect":false,"options":[{"label":"a"},{"label":"其他"}]}]});
        assert!(
            parse_questions(&other.to_string())
                .unwrap_err()
                .contains("其他")
        );
        let duplicate = serde_json::json!({"questions":[{"question":"q","header":"h","multiSelect":false,"options":[{"label":"a"},{"label":"A"}]}]});
        assert!(
            parse_questions(&duplicate.to_string())
                .unwrap_err()
                .contains("重复")
        );
    }

    #[test]
    fn answers_must_match_the_questions() {
        let questions = parse_questions(&sample_arguments()).unwrap();
        let ok = vec![
            UserQuestionAnswer {
                selected: vec!["SQLite（推荐）".to_string()],
                other: None,
            },
            UserQuestionAnswer {
                selected: vec!["macOS".to_string(), "Linux".to_string()],
                other: Some("  FreeBSD ".to_string()),
            },
        ];
        let normalized = validate_answers(&questions, &ok).unwrap();
        assert_eq!(normalized[1].other.as_deref(), Some("FreeBSD"));

        // 单选不能选两个，也不能同时选项加“其他”。
        let mut two = ok.clone();
        two[0].selected.push("Postgres".to_string());
        assert!(
            validate_answers(&questions, &two)
                .unwrap_err()
                .contains("单选")
        );
        let mut mixed = ok.clone();
        mixed[0].other = Some("MySQL".to_string());
        assert!(
            validate_answers(&questions, &mixed)
                .unwrap_err()
                .contains("单选")
        );
        // 只填“其他”是合法回答。
        let mut only_other = ok.clone();
        only_other[0] = UserQuestionAnswer {
            selected: vec![],
            other: Some("MySQL".to_string()),
        };
        assert!(validate_answers(&questions, &only_other).is_ok());
        // 选项不存在、没回答、数量不符都拒绝。
        let mut unknown = ok.clone();
        unknown[1].selected = vec!["BeOS".to_string()];
        assert!(
            validate_answers(&questions, &unknown)
                .unwrap_err()
                .contains("不存在")
        );
        let mut empty = ok.clone();
        empty[1] = UserQuestionAnswer::default();
        assert!(
            validate_answers(&questions, &empty)
                .unwrap_err()
                .contains("没有回答")
        );
        assert!(validate_answers(&questions, &ok[..1]).is_err());
    }

    #[test]
    fn resolve_wakes_the_waiter_once_and_scopes_to_the_session() {
        let registry = UserQuestionRegistry::default();
        let questions = parse_questions(&sample_arguments()).unwrap();
        let waiter = registry.request(pending(questions));
        assert_eq!(registry.pending_for_session(&SessionId::new("s1")).len(), 1);
        assert!(
            registry
                .pending_for_session(&SessionId::new("other"))
                .is_empty()
        );

        let answers = vec![
            UserQuestionAnswer {
                selected: vec!["Postgres".to_string()],
                other: None,
            },
            UserQuestionAnswer {
                selected: vec!["Windows".to_string()],
                other: None,
            },
        ];
        assert!(
            registry
                .resolve(
                    &SessionId::new("other"),
                    "q1",
                    UserQuestionResponse::Answered {
                        answers: answers.clone()
                    }
                )
                .is_err(),
            "别的会话不能回答"
        );
        registry
            .resolve(
                &SessionId::new("s1"),
                "q1",
                UserQuestionResponse::Answered {
                    answers: answers.clone(),
                },
            )
            .unwrap();
        assert_eq!(
            waiter.response_rx.recv().unwrap(),
            UserQuestionResponse::Answered { answers }
        );
        assert!(
            registry
                .resolve(&SessionId::new("s1"), "q1", UserQuestionResponse::Skipped)
                .is_err(),
            "已经收口的问题不能再回答"
        );
        assert!(
            registry
                .pending_for_session(&SessionId::new("s1"))
                .is_empty()
        );
    }

    #[test]
    fn turn_and_task_cleanup_drop_pending_questions() {
        let registry = UserQuestionRegistry::default();
        let questions = parse_questions(&sample_arguments()).unwrap();
        let _waiter = registry.request(pending(questions));
        registry.remove_turn(&SessionId::new("s1"), "turn-other");
        assert_eq!(registry.pending_for_session(&SessionId::new("s1")).len(), 1);
        registry.remove_turn(&SessionId::new("s1"), "turn1");
        assert!(
            registry
                .pending_for_session(&SessionId::new("s1"))
                .is_empty()
        );
    }
}
