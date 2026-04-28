use magi_core::{SessionId, UtcMillis};
use magi_session_store::NotificationRecord;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotificationItemDto {
    pub notification_id: String,
    pub message: String,
    pub kind: String,
    pub level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub handled: bool,
    pub read: bool,
    pub created_at: UtcMillis,
    pub persist_to_center: bool,
    pub action_required: bool,
    pub count_unread: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
}

impl From<NotificationRecord> for SessionNotificationItemDto {
    fn from(record: NotificationRecord) -> Self {
        let is_incident = record.kind == "incident";
        let count_unread = record
            .count_unread
            .unwrap_or(is_incident && !record.handled);
        Self {
            notification_id: record.notification_id,
            message: record.message,
            level: record.level.unwrap_or_else(|| {
                if is_incident {
                    "error".to_string()
                } else {
                    "info".to_string()
                }
            }),
            kind: record.kind,
            title: record.title,
            source: record.source,
            handled: record.handled,
            read: record.handled,
            created_at: record.created_at,
            persist_to_center: record.persist_to_center.unwrap_or(true),
            action_required: record.action_required.unwrap_or(is_incident),
            count_unread,
            display_mode: record.display_mode,
            duration: record.duration,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotificationSnapshotDto {
    pub last_updated_at: UtcMillis,
    pub records: Vec<SessionNotificationItemDto>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotificationsResponseDto {
    pub session_id: String,
    pub notifications: SessionNotificationSnapshotDto,
}

impl SessionNotificationsResponseDto {
    pub fn empty(session_id: Option<&SessionId>) -> Self {
        Self {
            session_id: session_id.map(ToString::to_string).unwrap_or_default(),
            notifications: SessionNotificationSnapshotDto {
                last_updated_at: UtcMillis::now(),
                records: Vec::new(),
            },
        }
    }

    pub fn from_records(session_id: &SessionId, records: Vec<NotificationRecord>) -> Self {
        Self {
            session_id: session_id.to_string(),
            notifications: SessionNotificationSnapshotDto {
                last_updated_at: UtcMillis::now(),
                records: records
                    .into_iter()
                    .map(SessionNotificationItemDto::from)
                    .collect(),
            },
        }
    }
}
