use magi_core::{SessionId, UtcMillis};
use magi_session_store::NotificationRecord;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotificationItemDto {
    pub notification_id: String,
    pub message: String,
    pub kind: String,
    pub handled: bool,
    pub created_at: UtcMillis,
}

impl From<NotificationRecord> for SessionNotificationItemDto {
    fn from(record: NotificationRecord) -> Self {
        Self {
            notification_id: record.notification_id,
            message: record.message,
            kind: record.kind,
            handled: record.handled,
            created_at: record.created_at,
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
