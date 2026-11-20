//! 插件会话事实面的目录项：共享宿主按此读取用户会话目录。
//!
//! 事实与 App Server 的 `SessionSummary` 同源（同一 `SessionRecord`、同一运行态判定），
//! 这里只多给宿主需要的创建目录；不另存状态，每次都从会话存储读取。
use crate::session_activity::session_running_task_count;
use magi_session_store::{SessionRecord, SessionRuntimeSidecar};
use serde::Serialize;

/// 一个用户会话的目录事实。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFact {
    pub session_id: String,
    pub workspace_id: Option<String>,
    /// 会话标题。还没有真实标题（仍是新建时的占位）时缺席，不把占位当标题。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `Active` 或 `Archived`（`SessionLifecycleStatus` 的名字）。
    pub status: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    pub is_running: bool,
    pub has_unread_completion: bool,
    /// 创建时选定的本机目录；没有则缺席，不用工作区根目录代替。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// 只有用户会话进入插件事实面；外部工具会话没有目录项。
pub fn session_fact(
    record: &SessionRecord,
    sidecar: Option<&SessionRuntimeSidecar>,
) -> Option<SessionFact> {
    if !record.kind.is_user() {
        return None;
    }
    let cwd = record
        .initial_working_directory_ref
        .as_deref()
        .and_then(|reference| magi_core::HostPath::from_path_ref(reference).ok())
        .and_then(|path| path.as_path().to_str().map(str::to_owned));
    Some(SessionFact {
        session_id: record.session_id.to_string(),
        workspace_id: record.workspace_id.clone(),
        title: (record.title != crate::session_title::NEW_SESSION_PLACEHOLDER_TITLE)
            .then(|| record.title.clone()),
        status: format!("{:?}", record.status),
        created_at: record.created_at.0,
        updated_at: record.updated_at.0,
        message_count: record.message_count.unwrap_or(0),
        is_running: session_running_task_count(sidecar) > 0,
        has_unread_completion: record.has_unread_completion(),
        cwd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{SessionId, SessionLifecycleStatus, UtcMillis};
    use magi_session_store::SessionKind;

    fn record(kind: SessionKind) -> SessionRecord {
        SessionRecord {
            session_id: SessionId::new("s1"),
            title: "标题".into(),
            status: SessionLifecycleStatus::Active,
            created_at: UtcMillis(10),
            updated_at: UtcMillis(20),
            message_count: Some(3),
            workspace_id: Some("w1".into()),
            initial_working_directory_ref: None,
            last_completed_at: Some(UtcMillis(15)),
            last_viewed_at: None,
            kind,
        }
    }

    #[test]
    fn user_sessions_carry_the_directory_facts_of_the_record() {
        let fact = session_fact(&record(SessionKind::User), None).expect("用户会话");
        assert_eq!(fact.session_id, "s1");
        assert_eq!(fact.title.as_deref(), Some("标题"));
        assert_eq!(fact.status, "Active");
        assert_eq!(
            (fact.created_at, fact.updated_at, fact.message_count),
            (10, 20, 3)
        );
        assert_eq!(fact.workspace_id.as_deref(), Some("w1"));
        assert!(!fact.is_running, "没有运行中的 Turn");
        assert!(fact.has_unread_completion, "完成时间晚于查看时间");
        assert_eq!(fact.cwd, None, "没有创建目录时不用工作区根目录代替");
    }

    #[test]
    fn a_placeholder_title_is_not_a_title() {
        let mut untitled = record(SessionKind::User);
        untitled.title = crate::session_title::NEW_SESSION_PLACEHOLDER_TITLE.into();
        let fact = session_fact(&untitled, None).expect("用户会话");
        assert_eq!(fact.title, None);
        assert!(!serde_json::to_string(&fact).unwrap().contains("title"));
    }

    #[test]
    fn external_tool_sessions_are_not_part_of_the_plugin_facts() {
        assert!(session_fact(&record(SessionKind::ExternalTool), None).is_none());
    }
}
