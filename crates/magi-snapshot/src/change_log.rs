use crate::error::{SnapshotError, SnapshotResult};
use crate::types::ChangeEvent;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// `events.log`：JSON Lines 格式，append-only。
///
/// 每行一个 `ChangeEvent`。崩溃恢复时按行扫描重放至最后一条完整行；
/// 任何被截断的尾部行直接丢弃。
pub struct ChangeLog {
    path: PathBuf,
    handle: Mutex<File>,
}

impl ChangeLog {
    pub fn open(path: impl Into<PathBuf>) -> SnapshotResult<Self> {
        let path: PathBuf = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| SnapshotError::io(parent, e))?;
        }
        let handle = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)
            .map_err(|e| SnapshotError::io(&path, e))?;
        Ok(Self {
            path,
            handle: Mutex::new(handle),
        })
    }

    pub fn append(&self, event: &ChangeEvent) -> SnapshotResult<()> {
        let mut payload = serde_json::to_vec(event)?;
        payload.push(b'\n');
        let mut handle = self.handle.lock().expect("change_log poisoned");
        handle
            .write_all(&payload)
            .map_err(|e| SnapshotError::io(&self.path, e))?;
        handle
            .flush()
            .map_err(|e| SnapshotError::io(&self.path, e))?;
        Ok(())
    }

    /// 全量读取所有完整事件行。
    pub fn read_all(&self) -> SnapshotResult<Vec<ChangeEvent>> {
        let f = File::open(&self.path).map_err(|e| SnapshotError::io(&self.path, e))?;
        let reader = BufReader::new(f);
        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|e| SnapshotError::io(&self.path, e))?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ChangeEvent>(&line) {
                Ok(ev) => events.push(ev),
                Err(err) => {
                    tracing::warn!(
                        path = %self.path.display(),
                        line_len = line.len(),
                        error = %err,
                        "discarding corrupt change_log line"
                    );
                }
            }
        }
        Ok(events)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChangeKind, SourceKind};
    use tempfile::tempdir;

    fn ev(id: &str) -> ChangeEvent {
        ChangeEvent {
            event_id: id.into(),
            timestamp_ms: 0,
            change_kind: ChangeKind::Modified,
            source: SourceKind::Tool,
            tool_call_id: None,
            worker_id: None,
            execution_group_id: None,
            before: None,
            after: None,
        }
    }

    #[test]
    fn append_and_read_round_trip() {
        let dir = tempdir().unwrap();
        let log = ChangeLog::open(dir.path().join("events.log")).unwrap();
        log.append(&ev("a")).unwrap();
        log.append(&ev("b")).unwrap();
        let all = log.read_all().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].event_id, "a");
    }

    #[test]
    fn corrupt_tail_is_skipped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log");
        {
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            let payload = serde_json::to_vec(&ev("a")).unwrap();
            f.write_all(&payload).unwrap();
            f.write_all(b"\n").unwrap();
            f.write_all(b"{\"event_id\":\"truncated\"").unwrap();
        }
        let log = ChangeLog::open(&path).unwrap();
        let all = log.read_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].event_id, "a");
    }
}
