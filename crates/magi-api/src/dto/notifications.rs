use magi_core::{SessionId, UtcMillis};
use magi_session_store::{NotificationRecord, NotificationScope};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncidentNotificationItemDto {
    pub notification_id: String,
    pub message: String,
    pub kind: &'static str,
    pub scope: NotificationScope,
    pub level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub handled: bool,
    pub read: bool,
    pub resolved: bool,
    pub created_at: UtcMillis,
    pub action_required: bool,
    pub count_unread: bool,
    pub occurrence_count: u32,
}

impl From<NotificationRecord> for IncidentNotificationItemDto {
    fn from(record: NotificationRecord) -> Self {
        Self {
            notification_id: record.notification_id,
            message: record.message,
            kind: "incident",
            scope: record.scope,
            level: record.level.unwrap_or_else(|| "error".to_string()),
            title: record.title,
            source: record.source,
            workspace_id: record.workspace_id,
            session_id: record.session_id.map(|session_id| session_id.to_string()),
            handled: record.handled,
            read: record.handled,
            resolved: record.resolved,
            created_at: record.created_at,
            action_required: record.action_required,
            count_unread: record.count_unread,
            occurrence_count: record.occurrence_count.max(1),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationCenterSnapshotDto {
    pub last_updated_at: UtcMillis,
    pub records: Vec<IncidentNotificationItemDto>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationsResponseDto {
    pub workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub notifications: NotificationCenterSnapshotDto,
}

impl NotificationsResponseDto {
    pub fn from_records(
        workspace_id: String,
        session_id: Option<&SessionId>,
        records: Vec<NotificationRecord>,
    ) -> Self {
        Self {
            workspace_id,
            session_id: session_id.map(ToString::to_string),
            notifications: NotificationCenterSnapshotDto {
                last_updated_at: UtcMillis::now(),
                records: records
                    .into_iter()
                    .map(IncidentNotificationItemDto::from)
                    .collect(),
            },
        }
    }
}
