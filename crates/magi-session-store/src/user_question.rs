//! 用户提问的持久合同与校验；运行时等待通道不属于持久事实。
use magi_core::{SessionId, TaskId, UtcMillis};
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn default_true() -> bool {
    true
}

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
    #[serde(default = "default_true")]
    pub allow_other: bool,
    pub options: Vec<UserQuestionOption>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingUserQuestion {
    pub question_id: String,
    pub session_id: SessionId,
    #[serde(default)]
    pub task_id: Option<TaskId>,
    pub turn_id: String,
    pub tool_call_id: String,
    pub questions: Vec<UserQuestion>,
    pub requested_at: UtcMillis,
    #[serde(default = "default_true")]
    pub can_skip: bool,
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
            allow_other: true,
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
        if other.is_some() && !question.allow_other {
            return Err(format!("第 {number} 个问题不支持自定义输入"));
        }
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
            // `answer` 是这道题用户最终的决定（预设选项与“其他”输入合并），模型直接按它行动。
            let mut decided: Vec<&str> = answer.selected.iter().map(String::as_str).collect();
            if let Some(other) = answer.other.as_deref() {
                decided.push(other);
            }
            serde_json::json!({
                "question": question.question,
                "header": question.header,
                "answer": decided,
                "selected": answer.selected,
                "other": answer.other,
                "custom_answer": answer.other.is_some(),
            })
        })
        .collect();
    serde_json::json!({
        "tool": "ask_user_question",
        "status": "answered",
        "answers": items,
        "instruction": "answer 就是用户对每道题的最终决定，直接按它继续。`other`/`custom_answer` 表示用户没有选你给的选项，而是自己写了答案：这是合法的决定，要按字面采纳，哪怕它不在你给出的选项范围内；不要因此拒绝、质疑或重复追问同一个问题。",
    })
}

pub const USER_QUESTION_METADATA_KEY: &str = "userQuestion";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserQuestionState {
    pub request: PendingUserQuestion,
    pub response: Option<UserQuestionResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<UtcMillis>,
}

impl UserQuestionState {
    pub fn result_payload(&self) -> Option<Value> {
        self.response.as_ref().map(|response| match response {
            UserQuestionResponse::Answered { answers } => answered_result_payload(&self.request.questions, answers),
            UserQuestionResponse::Skipped => serde_json::json!({
                "tool": "ask_user_question", "status": "skipped",
                "message": "用户选择不回答这些问题。不要反复追问：按你的最佳判断继续，并在回复里说明你做的假设。",
            }),
        })
    }
}
