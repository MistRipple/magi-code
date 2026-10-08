//! 内置浏览器的下载登记：daemon 记住最近的下载及其在 Desktop 私有目录里的位置，
//! 供 `browser_download` 工具列出并把已完成的文件复制进工作区。
//!
//! 文件位置只在 daemon 内部使用，从不出现在任何工具结果或面向用户的事件里：
//! 模型只能拿到 `download_id`、文件名、状态和大小。登记只在内存里，重启后随 Desktop 的
//! 下载目录一起清空。

use magi_core::BrowserTabId;
use serde::Serialize;
use std::{collections::VecDeque, path::PathBuf, sync::Mutex};

/// 登记里保留的下载条数上限，超过后淘汰最早的。
const MAX_DOWNLOADS: usize = 50;

#[derive(Clone, Debug, Serialize)]
pub struct BrowserDownloadSummary {
    pub download_id: String,
    pub tab_id: BrowserTabId,
    pub filename: String,
    pub state: String,
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
struct Entry {
    summary: BrowserDownloadSummary,
    saved_path: Option<PathBuf>,
}

/// 一次下载事件携带的事实。
pub struct BrowserDownloadUpdate {
    pub tab_id: BrowserTabId,
    pub download_id: String,
    pub filename: String,
    pub state: String,
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    pub saved_path: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct BrowserDownloadRegistry {
    entries: Mutex<VecDeque<Entry>>,
}

impl BrowserDownloadRegistry {
    pub fn record(&self, update: BrowserDownloadUpdate) {
        let mut entries = self.entries.lock().expect("browser download lock poisoned");
        let summary = BrowserDownloadSummary {
            download_id: update.download_id,
            tab_id: update.tab_id,
            filename: update.filename,
            state: update.state,
            received_bytes: update.received_bytes,
            total_bytes: update.total_bytes,
            error: update.error,
        };
        let saved_path = update.saved_path.map(PathBuf::from);
        if let Some(existing) = entries
            .iter_mut()
            .find(|entry| entry.summary.download_id == summary.download_id)
        {
            existing.summary = summary;
            if saved_path.is_some() {
                existing.saved_path = saved_path;
            }
            return;
        }
        entries.push_back(Entry {
            summary,
            saved_path,
        });
        while entries.len() > MAX_DOWNLOADS {
            entries.pop_front();
        }
    }

    /// 属于这些标签页的下载，按发生顺序（最新在后）。
    pub fn list_for_tabs(&self, tab_ids: &[BrowserTabId]) -> Vec<BrowserDownloadSummary> {
        self.entries
            .lock()
            .expect("browser download lock poisoned")
            .iter()
            .filter(|entry| tab_ids.contains(&entry.summary.tab_id))
            .map(|entry| entry.summary.clone())
            .collect()
    }

    /// 已完成下载的私有文件位置与文件名；下载不属于这些标签页、未完成或位置未知时返回 `None`。
    pub fn completed_file(
        &self,
        download_id: &str,
        tab_ids: &[BrowserTabId],
    ) -> Option<(PathBuf, String)> {
        self.entries
            .lock()
            .expect("browser download lock poisoned")
            .iter()
            .find(|entry| {
                entry.summary.download_id == download_id
                    && entry.summary.state == "completed"
                    && tab_ids.contains(&entry.summary.tab_id)
            })
            .and_then(|entry| {
                entry
                    .saved_path
                    .clone()
                    .map(|path| (path, entry.summary.filename.clone()))
            })
    }

    pub fn download_state(&self, download_id: &str, tab_ids: &[BrowserTabId]) -> Option<String> {
        self.entries
            .lock()
            .expect("browser download lock poisoned")
            .iter()
            .find(|entry| {
                entry.summary.download_id == download_id && tab_ids.contains(&entry.summary.tab_id)
            })
            .map(|entry| entry.summary.state.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(id: &str, tab: &str, state: &str, path: Option<&str>) -> BrowserDownloadUpdate {
        BrowserDownloadUpdate {
            tab_id: BrowserTabId::new(tab),
            download_id: id.to_string(),
            filename: format!("{id}.bin"),
            state: state.to_string(),
            received_bytes: 10,
            total_bytes: Some(10),
            saved_path: path.map(ToString::to_string),
            error: None,
        }
    }

    #[test]
    fn download_updates_are_merged_and_scoped_to_session_tabs() {
        let registry = BrowserDownloadRegistry::default();
        let mine = [BrowserTabId::new("tab-a")];
        registry.record(update("d1", "tab-a", "started", None));
        registry.record(update("d2", "tab-b", "completed", Some("/private/d2.bin")));
        assert_eq!(registry.list_for_tabs(&mine).len(), 1);
        assert!(
            registry.completed_file("d1", &mine).is_none(),
            "未完成的下载不能取文件"
        );

        registry.record(update("d1", "tab-a", "completed", Some("/private/d1.bin")));
        assert_eq!(
            registry.list_for_tabs(&mine).len(),
            1,
            "同一个下载只登记一次"
        );
        let (path, filename) = registry.completed_file("d1", &mine).expect("完成后可取");
        assert_eq!(path, PathBuf::from("/private/d1.bin"));
        assert_eq!(filename, "d1.bin");
        assert!(
            registry.completed_file("d2", &mine).is_none(),
            "其他会话标签页的下载不可见"
        );
    }

    #[test]
    fn registry_keeps_only_the_most_recent_downloads() {
        let registry = BrowserDownloadRegistry::default();
        let tabs = [BrowserTabId::new("tab")];
        for index in 0..(MAX_DOWNLOADS + 5) {
            registry.record(update(&format!("d{index}"), "tab", "completed", Some("/p")));
        }
        let listed = registry.list_for_tabs(&tabs);
        assert_eq!(listed.len(), MAX_DOWNLOADS);
        assert_eq!(listed[0].download_id, "d5", "最早的几条被淘汰");
    }

    #[test]
    fn summaries_never_expose_the_private_path() {
        let registry = BrowserDownloadRegistry::default();
        let tabs = [BrowserTabId::new("tab")];
        registry.record(update(
            "d1",
            "tab",
            "completed",
            Some("/very/private/path.bin"),
        ));
        let json = serde_json::to_string(&registry.list_for_tabs(&tabs)).expect("serialize");
        assert!(!json.contains("/very/private"), "{json}");
    }
}
