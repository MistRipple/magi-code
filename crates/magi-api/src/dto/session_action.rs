use magi_core::{EventId, SessionId, TaskId, UtcMillis};
use serde::Serialize;

#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub struct SessionActionImageDto {
    pub name: String,
    pub data_url: String,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub struct SessionActionRequestDto {
    pub session_id: Option<String>,
    pub text: Option<String>,
    pub deep_task: bool,
    pub skill_name: Option<String>,
    #[serde(default)]
    pub images: Vec<SessionActionImageDto>,
}

impl SessionActionRequestDto {
    pub fn requested_session_id(&self) -> Option<SessionId> {
        trimmed_non_empty(self.session_id.as_deref()).map(SessionId::new)
    }

    pub fn trimmed_text(&self) -> Option<String> {
        trimmed_non_empty(self.text.as_deref())
    }

    pub fn fallback_session_title(&self, trimmed_text: Option<&str>) -> String {
        trimmed_text
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(32).collect::<String>())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "新会话".to_string())
    }

    pub fn timeline_message(&self, trimmed_text: Option<&str>) -> String {
        let mut message_lines = Vec::new();

        if let Some(skill_name) = trimmed_non_empty(self.skill_name.as_deref()) {
            message_lines.push(format!("/{}", skill_name));
        }
        if let Some(text) = trimmed_text {
            message_lines.push(text.to_string());
        }
        if !self.images.is_empty() {
            message_lines.push(format!("[图片 {} 张]", self.images.len()));
        }
        if self.deep_task {
            message_lines.push("[深度任务]".to_string());
        }

        if message_lines.is_empty() {
            "[空输入]".to_string()
        } else {
            message_lines.join("\n")
        }
    }

}

#[derive(Clone, Debug, Serialize)]
pub struct SessionActionResponseDto {
    pub session_id: String,
    pub entry_id: String,
    pub event_id: String,
    pub accepted_at: UtcMillis,
    pub created_session: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_task_id: Option<String>,
}

impl SessionActionResponseDto {
    pub fn new(
        session_id: SessionId,
        entry_id: String,
        event_id: EventId,
        accepted_at: UtcMillis,
        created_session: bool,
        root_task_id: Option<TaskId>,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            entry_id,
            event_id: event_id.to_string(),
            accepted_at,
            created_session,
            root_task_id: root_task_id.map(|task_id| task_id.to_string()),
        }
    }
}

fn trimmed_non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> SessionActionRequestDto {
        SessionActionRequestDto {
            session_id: Some("  session-1  ".to_string()),
            text: Some("  hello world  ".to_string()),
            deep_task: true,
            skill_name: Some("  refactor  ".to_string()),
            images: vec![SessionActionImageDto {
                name: "image.png".to_string(),
                data_url: "data:image/png;base64,abc".to_string(),
            }],
        }
    }

    #[test]
    fn request_normalization_trims_textual_fields() {
        let request = request();

        assert_eq!(
            request.requested_session_id().as_ref().map(ToString::to_string),
            Some("session-1".to_string())
        );
        assert_eq!(request.trimmed_text().as_deref(), Some("hello world"));
    }

    #[test]
    fn fallback_session_title_uses_trimmed_text_prefix() {
        let request = request();
        let title = request.fallback_session_title(Some(
            "0123456789012345678901234567890123456789",
        ));

        assert_eq!(title, "01234567890123456789012345678901");
    }

    #[test]
    fn timeline_message_captures_skill_text_images_and_depth() {
        let request = request();
        let message = request.timeline_message(request.trimmed_text().as_deref());

        assert_eq!(message, "/refactor\nhello world\n[图片 1 张]\n[深度任务]");
    }

    #[test]
    fn timeline_message_falls_back_for_empty_input() {
        let request = SessionActionRequestDto {
            session_id: None,
            text: Some("   ".to_string()),
            deep_task: false,
            skill_name: Some("   ".to_string()),
            images: Vec::new(),
        };

        assert_eq!(request.timeline_message(None), "[空输入]");
        assert_eq!(request.fallback_session_title(None), "新会话");
    }

}
