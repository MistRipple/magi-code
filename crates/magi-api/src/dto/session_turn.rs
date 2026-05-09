use magi_core::{EventId, SessionId, TaskId, UtcMillis};
use magi_session_store::{CANONICAL_TURN_SCHEMA_VERSION, CanonicalTurn, CanonicalTurnItem};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTurnImageDto {
    pub name: String,
    #[serde(alias = "data_url")]
    pub data_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTurnRequestDto {
    #[serde(alias = "session_id")]
    pub session_id: Option<String>,
    #[serde(alias = "workspace_id")]
    pub workspace_id: Option<String>,
    #[serde(default, alias = "workspace_path")]
    pub workspace_path: Option<String>,
    pub text: Option<String>,
    #[serde(alias = "skill_name")]
    pub skill_name: Option<String>,
    #[serde(default)]
    pub images: Vec<SessionTurnImageDto>,
    #[serde(alias = "request_id")]
    pub request_id: Option<String>,
    #[serde(alias = "user_message_id")]
    pub user_message_id: Option<String>,
    #[serde(alias = "placeholder_message_id")]
    pub placeholder_message_id: Option<String>,
    /// 当为 true 时，本次 turn 直接作为补充上下文写入当前任务，
    /// 不进入分类器，也不创建新任务。
    #[serde(default, alias = "supplement_context")]
    pub supplement_context: bool,
    /// 当 `supplement_context` 为 true 时，可选指定写入哪个上下文任务；
    /// 缺省回退到当前 mission 的 root task。
    #[serde(default, alias = "context_task_id")]
    pub context_task_id: Option<String>,
}

impl SessionTurnRequestDto {
    pub fn mission_title(&self, trimmed_text: Option<&str>) -> String {
        trimmed_text
            .map(normalize_task_title)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "新任务".to_string())
    }

    pub fn requested_session_id(&self) -> Option<SessionId> {
        trimmed_non_empty(self.session_id.as_deref()).map(SessionId::new)
    }

    pub fn requested_workspace_id(&self) -> Option<String> {
        trimmed_non_empty(self.workspace_id.as_deref())
    }

    pub fn requested_workspace_path(&self) -> Option<String> {
        trimmed_non_empty(self.workspace_path.as_deref())
    }

    pub fn trimmed_text(&self) -> Option<String> {
        trimmed_non_empty(self.text.as_deref())
    }

    pub fn request_id(&self) -> Option<String> {
        trimmed_non_empty(self.request_id.as_deref())
    }

    pub fn user_message_id(&self) -> Option<String> {
        trimmed_non_empty(self.user_message_id.as_deref())
    }

    pub fn placeholder_message_id(&self) -> Option<String> {
        trimmed_non_empty(self.placeholder_message_id.as_deref())
    }

    pub fn timeline_message(&self, trimmed_text: Option<&str>) -> String {
        let mut message_lines = Vec::new();

        if let Some(skill_name) = trimmed_non_empty(self.skill_name.as_deref()) {
            message_lines.push(format!("/{skill_name}"));
        }
        if let Some(text) = trimmed_text {
            message_lines.push(text.to_string());
        }
        if !self.images.is_empty() {
            message_lines.push(format!("[图片 {} 张]", self.images.len()));
        }
        if message_lines.is_empty() {
            "[空输入]".to_string()
        } else {
            message_lines.join("\n")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTurnRouteDto {
    Chat,
    Execute,
    Task,
    Continue,
    SupplementContext,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTurnResponseDto {
    pub session_id: String,
    pub entry_id: String,
    pub event_id: String,
    pub accepted_at: UtcMillis,
    pub created_session: bool,
    pub route: SessionTurnRouteDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_chain_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_message_item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_schema_version: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_event_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_turn: Option<CanonicalTurn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_item: Option<CanonicalTurnItem>,
    /// 仅在 supplement_context 路由下返回：本次写入的上下文引用 ID。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_ref: Option<String>,
    /// 仅在 supplement_context 路由下返回：被写入的上下文任务 ID。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_task_id: Option<String>,
}

impl SessionTurnResponseDto {
    pub fn new(
        session_id: SessionId,
        entry_id: String,
        event_id: EventId,
        accepted_at: UtcMillis,
        created_session: bool,
        route: SessionTurnRouteDto,
        root_task_id: Option<TaskId>,
        action_task_id: Option<TaskId>,
        execution_chain_ref: Option<String>,
        user_message_item_id: Option<String>,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            entry_id,
            event_id: event_id.to_string(),
            accepted_at,
            created_session,
            route,
            root_task_id: root_task_id.map(|task_id| task_id.to_string()),
            action_task_id: action_task_id.map(|task_id| task_id.to_string()),
            execution_chain_ref,
            user_message_item_id,
            canonical_schema_version: None,
            canonical_event_kind: None,
            canonical_turn: None,
            canonical_item: None,
            context_ref: None,
            context_task_id: None,
        }
    }

    pub fn with_supplement_context(
        mut self,
        context_ref: String,
        context_task_id: String,
    ) -> Self {
        self.context_ref = Some(context_ref);
        self.context_task_id = Some(context_task_id);
        self
    }

    pub fn with_canonical_event(
        mut self,
        event_kind: &str,
        turn: Option<CanonicalTurn>,
        item: Option<CanonicalTurnItem>,
    ) -> Self {
        if turn.is_some() || item.is_some() {
            self.canonical_schema_version = Some(CANONICAL_TURN_SCHEMA_VERSION);
            self.canonical_event_kind = Some(event_kind.to_string());
            self.canonical_turn = turn;
            self.canonical_item = item;
        }
        self
    }
}

pub(crate) fn trimmed_non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(crate) fn normalize_task_title(value: &str) -> String {
    value
        .chars()
        .take(80)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_message_uses_user_text_directly() {
        let request = SessionTurnRequestDto {
            session_id: Some("session-a".to_string()),
            workspace_id: Some("workspace-a".to_string()),
            workspace_path: None,
            text: Some("请分析项目".to_string()),
            skill_name: None,
            images: Vec::new(),
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            supplement_context: false,
            context_task_id: None,
        };

        assert_eq!(
            request.timeline_message(request.trimmed_text().as_deref()),
            "请分析项目"
        );
    }
}
