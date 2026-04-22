use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyCategory {
    Audit,
    Incident,
    Progress,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyDisplayMode {
    NotificationCenter,
    Toast,
    Inline,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InternalControlNotice {
    pub title: String,
    pub content: String,
    pub level: NotifyLevel,
    pub category: NotifyCategory,
    pub source: String,
    pub display_mode: NotifyDisplayMode,
    pub persist_to_center: bool,
    pub action_required: bool,
    pub count_unread: bool,
}

pub struct InternalControlNoticeOptions {
    pub title: String,
    pub level: Option<NotifyLevel>,
    pub category: Option<NotifyCategory>,
    pub source: Option<String>,
    pub display_mode: Option<NotifyDisplayMode>,
    pub action_required: Option<bool>,
    pub count_unread: Option<bool>,
    pub persist_to_center: Option<bool>,
}

fn normalize_notice_content(content: &str) -> String {
    let trimmed = content.trim();
    if let Some(stripped) = trimmed.strip_prefix("[System]") {
        stripped.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn build_internal_control_notice(
    content: &str,
    options: &InternalControlNoticeOptions,
) -> Option<InternalControlNotice> {
    let normalized = normalize_notice_content(content);
    if normalized.is_empty() {
        return None;
    }
    let level = options.level.unwrap_or(NotifyLevel::Warning);
    Some(InternalControlNotice {
        title: options.title.clone(),
        content: normalized,
        level,
        category: options.category.unwrap_or(if level == NotifyLevel::Error {
            NotifyCategory::Incident
        } else {
            NotifyCategory::Audit
        }),
        source: options
            .source
            .clone()
            .unwrap_or_else(|| "orchestration-runtime".to_string()),
        display_mode: options
            .display_mode
            .unwrap_or(NotifyDisplayMode::NotificationCenter),
        persist_to_center: options.persist_to_center.unwrap_or(true),
        action_required: options
            .action_required
            .unwrap_or(level == NotifyLevel::Error),
        count_unread: options.count_unread.unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_options(title: &str) -> InternalControlNoticeOptions {
        InternalControlNoticeOptions {
            title: title.to_string(),
            level: None,
            category: None,
            source: None,
            display_mode: None,
            action_required: None,
            count_unread: None,
            persist_to_center: None,
        }
    }

    #[test]
    fn builds_notice_with_defaults() {
        let notice = build_internal_control_notice("test content", &default_options("Title")).unwrap();
        assert_eq!(notice.title, "Title");
        assert_eq!(notice.content, "test content");
        assert_eq!(notice.level, NotifyLevel::Warning);
        assert_eq!(notice.category, NotifyCategory::Audit);
        assert!(!notice.action_required);
    }

    #[test]
    fn strips_system_prefix() {
        let notice =
            build_internal_control_notice("[System] actual message", &default_options("T")).unwrap();
        assert_eq!(notice.content, "actual message");
    }

    #[test]
    fn empty_content_returns_none() {
        assert!(build_internal_control_notice("", &default_options("T")).is_none());
        assert!(build_internal_control_notice("  ", &default_options("T")).is_none());
    }

    #[test]
    fn error_level_sets_incident_category() {
        let mut opts = default_options("T");
        opts.level = Some(NotifyLevel::Error);
        let notice = build_internal_control_notice("err", &opts).unwrap();
        assert_eq!(notice.category, NotifyCategory::Incident);
        assert!(notice.action_required);
    }
}
