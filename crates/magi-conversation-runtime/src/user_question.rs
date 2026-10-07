//! 模型向用户提出的选择题（`ask_user_question`）的待处理注册表。
//!
//! 和工具授权同一类运行时事实：工具调用在执行线程里阻塞等待用户，注册表按会话保存待处理项，
//! 用户通过 API 回答后唤醒等待方。区别在于决定的形状（每题选中的选项 + “其他”自由输入），
//! 以及没有超时：用户可能需要想很久，只有回答、跳过、轮次结束或任务停止才会收口。
use magi_core::{SessionId, TaskId, UtcMillis};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};

pub const MAX_QUESTIONS: usize = 4;
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;
const MAX_TEXT_CHARS: usize = 2_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserQuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserQuestion {
    pub question: String,
    pub header: String,
    pub multi_select: bool,
    pub options: Vec<UserQuestionOption>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingUserQuestion {
    pub question_id: String,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub turn_id: String,
    pub tool_call_id: String,
    pub questions: Vec<UserQuestion>,
    pub requested_at: UtcMillis,
}

/// 用户对一道题的回答：选中的选项 label（可多个），以及“其他”里输入的原文。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserQuestionAnswer {
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserQuestionResponse {
    /// 按题目顺序给出每题的回答。
    Answered { answers: Vec<UserQuestionAnswer> },
    /// 用户选择不回答。
    Skipped,
}

/// 解析并校验工具参数。校验失败的原因直接返回给模型，让它改正后重试。
pub fn parse_questions(arguments: &str) -> Result<Vec<UserQuestion>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Raw {
        questions: Vec<RawQuestion>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawQuestion {
        question: String,
        header: String,
        #[serde(default)]
        multi_select: bool,
        options: Vec<RawOption>,
    }
    #[derive(Deserialize)]
    struct RawOption {
        label: String,
        #[serde(default)]
        description: String,
    }
    let raw: Raw = serde_json::from_str(arguments)
        .map_err(|error| format!("参数不是合法的 ask_user_question 输入：{error}"))?;
    if raw.questions.is_empty() || raw.questions.len() > MAX_QUESTIONS {
        return Err(format!("questions 需要 1–{MAX_QUESTIONS} 个问题"));
    }
    let mut questions = Vec::with_capacity(raw.questions.len());
    for (index, item) in raw.questions.into_iter().enumerate() {
        let number = index + 1;
        let question = item.question.trim().to_string();
        let header = item.header.trim().to_string();
        if question.is_empty() || header.is_empty() {
            return Err(format!("第 {number} 个问题的 question 与 header 不能为空"));
        }
        if question.chars().count() > MAX_TEXT_CHARS {
            return Err(format!("第 {number} 个问题的 question 过长"));
        }
        if item.options.len() < MIN_OPTIONS || item.options.len() > MAX_OPTIONS {
            return Err(format!(
                "第 {number} 个问题需要 {MIN_OPTIONS}–{MAX_OPTIONS} 个选项（“其他”由界面自动追加，不要自己写）"
            ));
        }
        let mut options: Vec<UserQuestionOption> = Vec::with_capacity(item.options.len());
        for option in item.options {
            let label = option.label.trim().to_string();
            if label.is_empty() {
                return Err(format!("第 {number} 个问题存在空的选项 label"));
            }
            if is_other_label(&label) {
                return Err(format!(
                    "第 {number} 个问题不要自己写“其他”选项，界面会自动追加"
                ));
            }
            if options
                .iter()
                .any(|existing| existing.label.eq_ignore_ascii_case(&label))
            {
                return Err(format!("第 {number} 个问题存在重复的选项 label：{label}"));
            }
            options.push(UserQuestionOption {
                label,
                description: option.description.trim().to_string(),
            });
        }
        questions.push(UserQuestion {
            question,
            header,
            multi_select: item.multi_select,
            options,
        });
    }
    Ok(questions)
}

fn is_other_label(label: &str) -> bool {
    let normalized = label.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "其他" | "其它" | "other" | "others" | "其他（请说明）"
    )
}

/// 校验用户的回答与题目对得上：数量一致、选中的 label 都在选项里、单选最多一个，
/// 且每题至少有一个选中项或“其他”输入。
pub fn validate_answers(
    questions: &[UserQuestion],
    answers: &[UserQuestionAnswer],
) -> Result<Vec<UserQuestionAnswer>, String> {
    if answers.len() != questions.len() {
        return Err(format!(
            "需要回答 {} 个问题，收到 {} 个回答",
            questions.len(),
            answers.len()
        ));
    }
    let mut normalized = Vec::with_capacity(answers.len());
    for (index, (question, answer)) in questions.iter().zip(answers).enumerate() {
        let number = index + 1;
        let other = answer
            .other
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        if let Some(text) = &other
            && text.chars().count() > MAX_TEXT_CHARS
        {
            return Err(format!("第 {number} 个问题的“其他”内容过长"));
        }
        let mut selected: Vec<String> = Vec::with_capacity(answer.selected.len());
        for label in &answer.selected {
            if !question.options.iter().any(|option| &option.label == label) {
                return Err(format!("第 {number} 个问题的选项不存在：{label}"));
            }
            if !selected.contains(label) {
                selected.push(label.clone());
            }
        }
        if selected.is_empty() && other.is_none() {
            return Err(format!("第 {number} 个问题还没有回答"));
        }
        if !question.multi_select && selected.len() + usize::from(other.is_some()) > 1 {
            return Err(format!("第 {number} 个问题是单选"));
        }
        normalized.push(UserQuestionAnswer { selected, other });
    }
    Ok(normalized)
}

/// 返回给模型的工具结果（成功回答）。
pub fn answered_result_payload(
    questions: &[UserQuestion],
    answers: &[UserQuestionAnswer],
) -> Value {
    let items: Vec<Value> = questions
        .iter()
        .zip(answers)
        .map(|(question, answer)| {
            serde_json::json!({
                "question": question.question,
                "header": question.header,
                "selected": answer.selected,
                "other": answer.other,
            })
        })
        .collect();
    serde_json::json!({
        "tool": "ask_user_question",
        "status": "answered",
        "answers": items,
    })
}

#[derive(Debug)]
struct PendingEntry {
    request: PendingUserQuestion,
    response_tx: mpsc::Sender<UserQuestionResponse>,
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

    pub fn request(&self, request: PendingUserQuestion) -> UserQuestionWaiter {
        let (response_tx, response_rx) = mpsc::channel();
        self.lock().insert(
            request.question_id.clone(),
            PendingEntry {
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
        let response = match response {
            UserQuestionResponse::Answered { answers } => UserQuestionResponse::Answered {
                answers: validate_answers(&entry.request.questions, &answers)?,
            },
            UserQuestionResponse::Skipped => UserQuestionResponse::Skipped,
        };
        let entry = pending.remove(question_id).expect("entry checked above");
        let _ = entry.response_tx.send(response);
        Ok(entry.request)
    }

    pub fn cancel(&self, question_id: &str) {
        self.lock().remove(question_id);
    }

    pub fn remove_turn(&self, session_id: &SessionId, turn_id: &str) {
        self.lock().retain(|_, entry| {
            !(&entry.request.session_id == session_id && entry.request.turn_id == turn_id)
        });
    }

    pub fn remove_task(&self, session_id: &SessionId, task_id: &TaskId) {
        self.lock().retain(|_, entry| {
            !(&entry.request.session_id == session_id && &entry.request.task_id == task_id)
        });
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
            task_id: TaskId::new("t1"),
            turn_id: "turn1".to_string(),
            tool_call_id: "call1".to_string(),
            questions,
            requested_at: UtcMillis(1),
        }
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
