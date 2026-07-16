use magi_conversation_runtime::context_reference::{
    SessionContextReference, SessionContextReferenceKind,
};
use magi_conversation_runtime::session_images::SessionTurnImage;
use magi_core::{AccessProfile, EventId, SessionId, TaskId, UtcMillis};
use magi_session_store::{CANONICAL_TURN_SCHEMA_VERSION, CanonicalTurn, CanonicalTurnItem};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTurnImageDto {
    pub name: String,
    pub data_url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionContextReferenceKindDto {
    File,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionContextReferenceDto {
    pub kind: SessionContextReferenceKindDto,
    pub path: String,
    #[serde(default)]
    pub path_ref: Option<String>,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTurnRequestDto {
    pub session_id: Option<String>,
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub workspace_path: Option<String>,
    pub text: Option<String>,
    pub skill_name: Option<String>,
    #[serde(default)]
    pub goal_mode: bool,
    #[serde(default)]
    pub images: Vec<SessionTurnImageDto>,
    #[serde(default)]
    pub context_references: Vec<SessionContextReferenceDto>,
    #[serde(default)]
    pub access_profile: Option<AccessProfile>,
    #[serde(default)]
    pub orchestrator_session_config: Option<Value>,
    pub request_id: Option<String>,
    pub user_message_id: Option<String>,
    pub placeholder_message_id: Option<String>,
    /// 当为 true 时，本次输入作为引导追加到当前活跃 Turn，不进入分类器，
    /// 也不创建新的独立 Turn。
    #[serde(default)]
    pub steer_current_turn: bool,
    /// 引导必须绑定调用方观察到的当前 Turn，防止完成边界后的迟到输入串入下一轮。
    #[serde(default)]
    pub expected_turn_id: Option<String>,
}

impl SessionTurnRequestDto {
    pub fn validate_context_references(&mut self) -> Result<Vec<SessionContextReference>, String> {
        const MAX_CONTEXT_REFERENCES: usize = 20;
        if self.context_references.len() > MAX_CONTEXT_REFERENCES {
            return Err(format!(
                "单轮最多添加 {MAX_CONTEXT_REFERENCES} 个文件或文件夹引用"
            ));
        }

        let mut validated = Vec::new();
        for reference in self.context_references.clone() {
            let path = if let Some(path_ref) = reference
                .path_ref
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                magi_core::HostPath::from_path_ref(path_ref)
                    .map_err(|_| "上下文引用路径引用无效".to_string())?
                    .into_path_buf()
            } else {
                let raw_path = reference.path.trim();
                if raw_path.is_empty() {
                    return Err("上下文引用路径不能为空".to_string());
                }
                std::path::PathBuf::from(raw_path)
            };
            if !path.is_absolute() {
                return Err("上下文引用必须使用绝对路径".to_string());
            }
            let canonical = path
                .canonicalize()
                .map_err(|_| format!("上下文引用不可用: {}", path.display()))?;
            let kind = match reference.kind {
                SessionContextReferenceKindDto::File if canonical.is_file() => {
                    SessionContextReferenceKind::File
                }
                SessionContextReferenceKindDto::Directory if canonical.is_dir() => {
                    SessionContextReferenceKind::Directory
                }
                SessionContextReferenceKindDto::File => {
                    return Err(format!("引用路径不是文件: {}", canonical.display()));
                }
                SessionContextReferenceKindDto::Directory => {
                    return Err(format!("引用路径不是文件夹: {}", canonical.display()));
                }
            };
            if validated
                .iter()
                .any(|existing: &SessionContextReference| existing.path == canonical)
            {
                continue;
            }
            let name = reference.name.trim().to_string();
            let name = if name.is_empty() {
                canonical
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
                    .unwrap_or_else(|| canonical.display().to_string())
            } else {
                name
            };
            validated.push(SessionContextReference {
                kind,
                path: canonical,
                name,
            });
        }
        self.context_references = validated
            .iter()
            .map(|reference| SessionContextReferenceDto {
                kind: match reference.kind {
                    SessionContextReferenceKind::File => SessionContextReferenceKindDto::File,
                    SessionContextReferenceKind::Directory => {
                        SessionContextReferenceKindDto::Directory
                    }
                },
                path: reference.path.display().to_string(),
                path_ref: Some(
                    magi_core::HostPath::from_path(reference.path.clone())
                        .to_path_ref()
                        .as_str()
                        .to_string(),
                ),
                name: reference.name.clone(),
            })
            .collect();
        Ok(validated)
    }

    pub fn context_references(&self) -> Vec<SessionContextReference> {
        self.context_references
            .iter()
            .map(|reference| SessionContextReference {
                kind: match reference.kind {
                    SessionContextReferenceKindDto::File => SessionContextReferenceKind::File,
                    SessionContextReferenceKindDto::Directory => {
                        SessionContextReferenceKind::Directory
                    }
                },
                path: reference
                    .path_ref
                    .as_deref()
                    .and_then(|value| magi_core::HostPath::from_path_ref(value).ok())
                    .map(magi_core::HostPath::into_path_buf)
                    .unwrap_or_else(|| std::path::PathBuf::from(&reference.path)),
                name: reference.name.clone(),
            })
            .collect()
    }

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

    pub fn requested_access_profile(&self) -> AccessProfile {
        self.access_profile.unwrap_or_default()
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

    pub fn expected_turn_id(&self) -> Option<String> {
        trimmed_non_empty(self.expected_turn_id.as_deref())
    }

    pub fn timeline_message(&self, trimmed_text: Option<&str>) -> String {
        let mut message_lines = Vec::new();

        if let Some(text) = trimmed_text {
            message_lines.push(text.to_string());
        }
        if !self.images.is_empty() {
            message_lines.push(format!("[图片 {} 张]", self.images.len()));
        }
        if message_lines.is_empty() && !self.context_references.is_empty() {
            message_lines.push(format!("[上下文引用 {} 项]", self.context_references.len()));
        }
        if message_lines.is_empty() {
            "[空输入]".to_string()
        } else {
            message_lines.join("\n")
        }
    }

    pub fn parsed_images(&self) -> Result<Vec<SessionTurnImage>, String> {
        self.images
            .iter()
            .map(|image| {
                SessionTurnImage::from_data_url(image.name.clone(), image.data_url.clone())
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTurnRouteDto {
    Chat,
    Execute,
    Task,
    Continue,
    Steer,
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
    pub queued: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_schema_version: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_event_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_turn: Option<CanonicalTurn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_item: Option<CanonicalTurnItem>,
    /// 仅在 steer 路由下返回：本次引导实际写入的活跃 Turn ID。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steered_turn_id: Option<String>,
}

pub struct SessionTurnResponseInput {
    pub session_id: SessionId,
    pub entry_id: String,
    pub event_id: EventId,
    pub accepted_at: UtcMillis,
    pub created_session: bool,
    pub route: SessionTurnRouteDto,
    pub root_task_id: Option<TaskId>,
    pub action_task_id: Option<TaskId>,
    pub execution_chain_ref: Option<String>,
    pub user_message_item_id: Option<String>,
}

impl SessionTurnResponseDto {
    pub fn new(input: SessionTurnResponseInput) -> Self {
        let SessionTurnResponseInput {
            session_id,
            entry_id,
            event_id,
            accepted_at,
            created_session,
            route,
            root_task_id,
            action_task_id,
            execution_chain_ref,
            user_message_item_id,
        } = input;
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
            queued: false,
            queue_id: None,
            queue_position: None,
            canonical_schema_version: None,
            canonical_event_kind: None,
            canonical_turn: None,
            canonical_item: None,
            steered_turn_id: None,
        }
    }

    pub fn with_queued(mut self, queue_id: String, queue_position: usize) -> Self {
        self.queued = true;
        self.queue_id = Some(queue_id);
        self.queue_position = Some(queue_position);
        self
    }

    pub fn with_steered_turn(mut self, turn_id: String) -> Self {
        self.steered_turn_id = Some(turn_id);
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
            goal_mode: false,
            images: Vec::new(),
            context_references: Vec::new(),
            access_profile: None,
            orchestrator_session_config: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            steer_current_turn: false,
            expected_turn_id: None,
        };

        assert_eq!(
            request.timeline_message(request.trimmed_text().as_deref()),
            "请分析项目"
        );
    }

    #[test]
    fn session_turn_request_accepts_camel_case_access_profile() {
        let request: SessionTurnRequestDto = serde_json::from_value(serde_json::json!({
            "text": "请执行当前任务",
            "images": [],
            "accessProfile": "full_access"
        }))
        .expect("request should parse");

        assert_eq!(
            request.requested_access_profile(),
            magi_core::AccessProfile::FullAccess
        );
    }

    #[test]
    fn session_turn_request_accepts_structured_goal_mode() {
        let request: SessionTurnRequestDto = serde_json::from_value(serde_json::json!({
            "text": "完成当前产品稳定性验收",
            "images": [],
            "goalMode": true
        }))
        .expect("request should parse structured goal mode");

        assert!(request.goal_mode);
    }

    #[test]
    fn timeline_message_keeps_control_fields_out_of_user_content() {
        let request: SessionTurnRequestDto = serde_json::from_value(serde_json::json!({
            "text": "完成稳定性验收",
            "skillName": "cn-engineering-standard",
            "goalMode": true,
            "images": []
        }))
        .expect("combined goal and skill request should parse");

        assert_eq!(
            request.timeline_message(request.trimmed_text().as_deref()),
            "完成稳定性验收"
        );
    }

    #[test]
    fn session_turn_request_accepts_structured_context_references() {
        let request: SessionTurnRequestDto = serde_json::from_value(serde_json::json!({
            "text": "分析引用的文件",
            "images": [],
            "contextReferences": [
                {
                    "kind": "file",
                    "path": "/tmp/reference.md",
                    "name": "reference.md"
                },
                {
                    "kind": "directory",
                    "path": "/tmp/reference-dir",
                    "name": "reference-dir"
                }
            ]
        }))
        .expect("structured context references should parse");

        assert_eq!(request.context_references.len(), 2);
        assert_eq!(request.context_references[0].path, "/tmp/reference.md");
        assert_eq!(
            request.context_references[1].kind,
            SessionContextReferenceKindDto::Directory
        );
        assert_eq!(
            request.timeline_message(request.trimmed_text().as_deref()),
            "分析引用的文件",
            "structured references must stay out of user-authored content"
        );
    }

    #[test]
    fn context_references_are_canonicalized_deduplicated_and_type_checked() {
        let root =
            std::env::temp_dir().join(format!("magi-context-reference-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("folder")).expect("reference directory should create");
        std::fs::write(root.join("file.md"), "reference\n").expect("reference file should create");

        let mut request: SessionTurnRequestDto = serde_json::from_value(serde_json::json!({
            "text": "验证引用",
            "contextReferences": [
                { "kind": "file", "path": root.join("file.md"), "name": "file.md" },
                { "kind": "file", "path": root.join("file.md"), "name": "duplicate.md" },
                { "kind": "directory", "path": root.join("folder"), "name": "folder" }
            ]
        }))
        .expect("context reference request should parse");

        let references = request
            .validate_context_references()
            .expect("valid references should canonicalize");
        assert_eq!(references.len(), 2);
        assert_eq!(references[0].name, "file.md");
        assert!(references[0].path.is_absolute());
        assert_eq!(references[1].kind.as_str(), "directory");
        assert_eq!(request.context_references.len(), 2);
        assert_eq!(
            request.context_references[0].path,
            references[0].path.display().to_string(),
            "queued requests must retain the canonical reference facts"
        );

        let mut invalid: SessionTurnRequestDto = serde_json::from_value(serde_json::json!({
            "text": "验证错误类型",
            "contextReferences": [
                { "kind": "directory", "path": root.join("file.md"), "name": "file.md" }
            ]
        }))
        .expect("invalid reference kind should still parse transport payload");
        assert!(invalid.validate_context_references().is_err());

        std::fs::remove_dir_all(root).expect("reference fixture should clean up");
    }

    #[test]
    fn context_reference_accepts_host_path_ref_as_authoritative_path() {
        let root = std::env::temp_dir().join(format!(
            "magi-context-reference-path-ref-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("reference directory should create");
        let path_ref = magi_core::HostPath::from_path(root.clone())
            .to_path_ref()
            .as_str()
            .to_string();
        let mut request: SessionTurnRequestDto = serde_json::from_value(serde_json::json!({
            "text": "inspect",
            "contextReferences": [{
                "kind": "directory",
                "path": "display-only",
                "pathRef": path_ref,
                "name": "reference"
            }]
        }))
        .expect("request should deserialize");

        let references = request
            .validate_context_references()
            .expect("path ref should validate");
        assert_eq!(references[0].path, root.canonicalize().unwrap());
    }
}
