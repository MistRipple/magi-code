use crate::{EventCategory, EventContext, EventEnvelope};
use magi_core::UtcMillis;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use thiserror::Error;

pub const AUDIT_USAGE_LEDGER_SCHEMA_VERSION: &str = "audit-usage-ledger-v1";
/// 活动段超过该大小后，新条目写入新段。段是保留策略的删除单位。
pub const AUDIT_USAGE_LEDGER_SEGMENT_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// 启动加载时删除最新条目早于该期限的整段（最新一段始终保留，用于延续序号）。
pub const AUDIT_USAGE_LEDGER_RETENTION_MILLIS: u64 = 180 * 24 * 60 * 60 * 1000;
/// 「重置执行统计」的标记事件：统计只计入最后一次重置之后的模型用量。
pub const USAGE_STATS_RESET_EVENT_TYPE: &str = "usage.stats.reset";

const SEGMENT_PREFIX: &str = "segment-";
const SEGMENT_EXTENSION: &str = "jsonl";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LedgerCategory {
    Audit,
    Usage,
}

#[derive(Serialize)]
struct LedgerLineRef<'a> {
    category: LedgerCategory,
    #[serde(flatten)]
    entry: &'a AuditUsageLedgerEntry,
}

#[derive(Deserialize)]
struct LedgerLine {
    category: LedgerCategory,
    #[serde(flatten)]
    entry: AuditUsageLedgerEntry,
}

/// 一次增量刷盘要追加的内容：sequence 大于已落盘水位的全部条目。
#[derive(Debug)]
pub struct AuditUsageLedgerAppend {
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditUsageLedgerEntry {
    pub event_id: String,
    pub event_type: String,
    pub occurred_at: UtcMillis,
    pub sequence: u64,
    pub context: EventContext,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditUsageLedgerSnapshot {
    pub schema_version: String,
    pub next_sequence: u64,
    pub audit_entries: Vec<AuditUsageLedgerEntry>,
    pub usage_entries: Vec<AuditUsageLedgerEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AuditUsageLedgerStatus {
    pub schema_version: String,
    pub next_sequence: u64,
    pub audit_count: usize,
    pub usage_count: usize,
    pub persistence_path: Option<PathBuf>,
    pub last_persist_error: Option<String>,
}

#[derive(Debug, Error)]
pub enum AuditUsageLedgerError {
    #[error("审计/用量账本 JSON 处理失败: {0}")]
    Json(#[from] serde_json::Error),
    #[error("审计/用量账本 IO 失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("审计/用量账本 schema 版本不匹配: expected {expected}, actual {actual}")]
    SchemaMismatch { expected: String, actual: String },
    #[error("审计/用量账本段损坏，拒绝以不完整账本继续 {path}: {reason}")]
    CorruptSegment { path: PathBuf, reason: String },
}

impl Default for AuditUsageLedgerSnapshot {
    fn default() -> Self {
        Self {
            schema_version: AUDIT_USAGE_LEDGER_SCHEMA_VERSION.to_string(),
            next_sequence: 1,
            audit_entries: Vec::new(),
            usage_entries: Vec::new(),
        }
    }
}

impl AuditUsageLedgerSnapshot {
    pub fn from_events(events: &[EventEnvelope]) -> Self {
        let mut snapshot = Self::default();
        for event in events {
            snapshot.record_event(event);
        }
        snapshot.normalize()
    }

    pub fn record_event(&mut self, event: &EventEnvelope) {
        let entry = AuditUsageLedgerEntry::from_event(event);
        match event.category {
            EventCategory::Audit => self.audit_entries.push(entry),
            EventCategory::Usage => self.usage_entries.push(entry),
            _ => return,
        }
        self.next_sequence = self.next_sequence.max(event.sequence.saturating_add(1));
    }

    pub fn normalize(mut self) -> Self {
        normalize_entries(&mut self.audit_entries);
        normalize_entries(&mut self.usage_entries);

        let highest_sequence = self
            .audit_entries
            .iter()
            .chain(self.usage_entries.iter())
            .map(|entry| entry.sequence)
            .max()
            .unwrap_or(0);
        self.next_sequence = self
            .next_sequence
            .max(highest_sequence.saturating_add(1))
            .max(1);
        self
    }

    /// 收集 sequence 大于 `watermark` 的条目，按 sequence 顺序编码为待追加的 JSONL。
    ///
    /// 存活账本的两个数组都按 sequence 递增（发布时在事件锁内分配序号并追加，
    /// 恢复时已规范化），所以用二分定位增量起点，不扫描历史。
    pub fn append_after(
        &self,
        watermark: u64,
    ) -> Result<Option<AuditUsageLedgerAppend>, AuditUsageLedgerError> {
        let audit = &self.audit_entries[self
            .audit_entries
            .partition_point(|entry| entry.sequence <= watermark)..];
        let usage = &self.usage_entries[self
            .usage_entries
            .partition_point(|entry| entry.sequence <= watermark)..];
        if audit.is_empty() && usage.is_empty() {
            return Ok(None);
        }
        let mut content = String::new();
        let (mut audit_index, mut usage_index) = (0, 0);
        let (mut first_sequence, mut last_sequence) = (u64::MAX, 0);
        while audit_index < audit.len() || usage_index < usage.len() {
            let take_audit = match (audit.get(audit_index), usage.get(usage_index)) {
                (Some(audit_entry), Some(usage_entry)) => {
                    audit_entry.sequence <= usage_entry.sequence
                }
                (Some(_), None) => true,
                _ => false,
            };
            let (category, entry) = if take_audit {
                audit_index += 1;
                (LedgerCategory::Audit, &audit[audit_index - 1])
            } else {
                usage_index += 1;
                (LedgerCategory::Usage, &usage[usage_index - 1])
            };
            first_sequence = first_sequence.min(entry.sequence);
            last_sequence = last_sequence.max(entry.sequence);
            content.push_str(&serde_json::to_string(&LedgerLineRef { category, entry })?);
            content.push('\n');
        }
        Ok(Some(AuditUsageLedgerAppend {
            first_sequence,
            last_sequence,
            content,
        }))
    }

    /// 把增量追加到段目录的活动段；活动段已满时以首个 sequence 新建一段。
    pub fn append_to_dir(
        dir: &Path,
        append: &AuditUsageLedgerAppend,
    ) -> Result<(), AuditUsageLedgerError> {
        fs::create_dir_all(dir)?;
        let active = segment_paths(dir)?.pop();
        let target = match active {
            Some(path) if fs::metadata(&path)?.len() < AUDIT_USAGE_LEDGER_SEGMENT_MAX_BYTES => path,
            _ => dir.join(segment_file_name(append.first_sequence)),
        };
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)?;
        file.write_all(append.content.as_bytes())?;
        file.sync_data()?;
        Ok(())
    }

    /// 从段目录加载账本。
    ///
    /// 只有最后一段允许以不完整的行结尾（追加写在行中途崩溃），按 WAL 惯例截断到
    /// 最后一个完整行；其余任何无法解析的内容都视为损坏并拒绝继续。最新条目早于
    /// 保留期的整段在这里删除，最新一段始终保留以延续序号。
    pub fn load_from_dir(dir: &Path, now: UtcMillis) -> Result<Self, AuditUsageLedgerError> {
        let mut snapshot = Self::default();
        if !dir.exists() {
            return Ok(snapshot);
        }
        let paths = segment_paths(dir)?;
        let retention_cutoff = now.0.saturating_sub(AUDIT_USAGE_LEDGER_RETENTION_MILLIS);
        let last_index = paths.len().saturating_sub(1);
        for (index, path) in paths.iter().enumerate() {
            let mut bytes = fs::read(path)?;
            if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
                if index != last_index {
                    return Err(AuditUsageLedgerError::CorruptSegment {
                        path: path.clone(),
                        reason: "非活动段以不完整的行结尾".to_string(),
                    });
                }
                let complete = bytes
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map_or(0, |position| position + 1);
                bytes.truncate(complete);
                fs::OpenOptions::new()
                    .write(true)
                    .open(path)?
                    .set_len(complete as u64)?;
            }
            let mut lines = Vec::new();
            for (line_index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
                if line.is_empty() {
                    continue;
                }
                let line: LedgerLine = serde_json::from_slice(line).map_err(|error| {
                    AuditUsageLedgerError::CorruptSegment {
                        path: path.clone(),
                        reason: format!("第 {} 行无法解析: {error}", line_index + 1),
                    }
                })?;
                lines.push(line);
            }
            let newest = lines.iter().map(|line| line.entry.occurred_at.0).max();
            if index != last_index && newest.is_none_or(|newest| newest < retention_cutoff) {
                fs::remove_file(path)?;
                continue;
            }
            for line in lines {
                snapshot.next_sequence = snapshot
                    .next_sequence
                    .max(line.entry.sequence.saturating_add(1));
                match line.category {
                    LedgerCategory::Audit => snapshot.audit_entries.push(line.entry),
                    LedgerCategory::Usage => snapshot.usage_entries.push(line.entry),
                }
            }
        }
        Ok(snapshot.normalize())
    }

    /// 已落盘的最大 sequence；恢复后以它作为增量刷盘水位。
    pub fn last_sequence(&self) -> u64 {
        let last =
            |entries: &[AuditUsageLedgerEntry]| entries.last().map_or(0, |entry| entry.sequence);
        last(&self.audit_entries).max(last(&self.usage_entries))
    }

    pub fn validate_schema(&self) -> Result<(), AuditUsageLedgerError> {
        if self.schema_version != AUDIT_USAGE_LEDGER_SCHEMA_VERSION {
            return Err(AuditUsageLedgerError::SchemaMismatch {
                expected: AUDIT_USAGE_LEDGER_SCHEMA_VERSION.to_string(),
                actual: self.schema_version.clone(),
            });
        }
        Ok(())
    }

    pub fn audit_count(&self) -> usize {
        self.audit_entries.len()
    }

    pub fn usage_count(&self) -> usize {
        self.usage_entries.len()
    }

    pub fn status(
        &self,
        persistence_path: Option<&Path>,
        last_persist_error: Option<String>,
    ) -> AuditUsageLedgerStatus {
        AuditUsageLedgerStatus {
            schema_version: self.schema_version.clone(),
            next_sequence: self.next_sequence,
            audit_count: self.audit_count(),
            usage_count: self.usage_count(),
            persistence_path: persistence_path.map(Path::to_path_buf),
            last_persist_error,
        }
    }
}

impl AuditUsageLedgerEntry {
    fn from_event(event: &EventEnvelope) -> Self {
        Self {
            event_id: event.event_id.to_string(),
            event_type: event.event_type.clone(),
            occurred_at: event.occurred_at,
            sequence: event.sequence,
            context: EventContext {
                workspace_id: event.workspace_id.clone(),
                session_id: event.session_id.clone(),
                mission_id: event.mission_id.clone(),
                assignment_id: event.assignment_id.clone(),
                task_id: event.task_id.clone(),
            },
            payload: event.payload.clone(),
        }
    }
}

fn segment_file_name(first_sequence: u64) -> String {
    format!("{SEGMENT_PREFIX}{first_sequence:020}.{SEGMENT_EXTENSION}")
}

fn segment_paths(dir: &Path) -> Result<Vec<PathBuf>, AuditUsageLedgerError> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let is_segment = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.strip_prefix(SEGMENT_PREFIX)
                    .and_then(|rest| rest.strip_suffix(&format!(".{SEGMENT_EXTENSION}")))
                    .is_some_and(|digits| {
                        digits.len() == 20 && digits.bytes().all(|b| b.is_ascii_digit())
                    })
            });
        if is_segment {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn normalize_entries(entries: &mut Vec<AuditUsageLedgerEntry>) {
    let mut seen = HashSet::<String>::new();
    entries.retain(|entry| seen.insert(entry.event_id.clone()));
    entries.sort_by(|left, right| {
        left.sequence
            .cmp(&right.sequence)
            .then(left.event_id.cmp(&right.event_id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventCategory, EventEnvelope};
    use magi_core::EventId;
    use serde_json::json;
    use std::fs;
    use std::io::Write;

    fn event(category: EventCategory, event_type: &str, sequence: u64) -> EventEnvelope {
        let mut event = match category {
            EventCategory::Domain => EventEnvelope::domain(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
            EventCategory::Audit => EventEnvelope::audit(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
            EventCategory::Usage => EventEnvelope::usage(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
            EventCategory::Projection => EventEnvelope::projection(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
            EventCategory::System => EventEnvelope::system(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
        };
        event.sequence = sequence;
        event
    }

    #[test]
    fn 从事件导出账本时只收口审计与用量事件() {
        let snapshot = AuditUsageLedgerSnapshot::from_events(&[
            event(EventCategory::Domain, "mission.created", 1),
            event(EventCategory::Audit, "ledger.audit.recorded", 2),
            event(EventCategory::Usage, "ledger.usage.recorded", 3),
        ]);

        assert_eq!(snapshot.audit_count(), 1);
        assert_eq!(snapshot.usage_count(), 1);
        assert_eq!(snapshot.next_sequence, 4);
        assert_eq!(
            snapshot.audit_entries[0].event_type,
            "ledger.audit.recorded"
        );
        assert_eq!(
            snapshot.usage_entries[0].event_type,
            "ledger.usage.recorded"
        );
    }

    fn temp_dir(label: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "magi-event-bus-ledger-{label}-{}-{}",
            std::process::id(),
            UtcMillis::now().0
        ));
        let _ = fs::remove_dir_all(&base);
        base
    }

    fn segment_files(dir: &Path) -> Vec<String> {
        segment_paths(dir)
            .expect("list segments")
            .into_iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn flush(snapshot: &AuditUsageLedgerSnapshot, dir: &Path, watermark: u64) -> u64 {
        match snapshot.append_after(watermark).expect("encode append") {
            Some(append) => {
                AuditUsageLedgerSnapshot::append_to_dir(dir, &append).expect("append");
                append.last_sequence
            }
            None => watermark,
        }
    }

    #[test]
    fn 增量刷盘只追加水位之后的条目并按序号交错恢复() {
        let dir = temp_dir("append");
        let mut snapshot = AuditUsageLedgerSnapshot::default();
        snapshot.record_event(&event(EventCategory::Audit, "ledger.audit.recorded", 2));
        snapshot.record_event(&event(EventCategory::Usage, "ledger.usage.recorded", 3));
        let watermark = flush(&snapshot, &dir, 0);
        assert_eq!(watermark, 3);
        let first_len = fs::metadata(dir.join(segment_file_name(2))).unwrap().len();

        snapshot.record_event(&event(EventCategory::Usage, "ledger.usage.recorded", 4));
        snapshot.record_event(&event(EventCategory::Audit, "ledger.audit.recorded", 5));
        assert!(snapshot.append_after(5).unwrap().is_none());
        let append = snapshot.append_after(watermark).unwrap().unwrap();
        assert_eq!((append.first_sequence, append.last_sequence), (4, 5));
        assert_eq!(append.content.lines().count(), 2);
        AuditUsageLedgerSnapshot::append_to_dir(&dir, &append).unwrap();

        // 第二次刷盘只在原段尾部追加两行，没有重写已有内容。
        let content = fs::read_to_string(dir.join(segment_file_name(2))).unwrap();
        assert_eq!(content.lines().count(), 4);
        assert!(content.len() as u64 > first_len);

        let restored = AuditUsageLedgerSnapshot::load_from_dir(&dir, UtcMillis::now()).unwrap();
        assert_eq!(restored.audit_count(), 2);
        assert_eq!(restored.usage_count(), 2);
        assert_eq!(restored.next_sequence, 6);
        assert_eq!(restored.last_sequence(), 5);
        assert_eq!(restored.audit_entries[1].sequence, 5);
    }

    #[test]
    fn 活动段写满后新建以首个序号命名的段() {
        let dir = temp_dir("rotate");
        fs::create_dir_all(&dir).unwrap();
        let full = dir.join(segment_file_name(1));
        fs::write(&full, "").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&full)
            .unwrap()
            .set_len(AUDIT_USAGE_LEDGER_SEGMENT_MAX_BYTES)
            .unwrap();
        let mut snapshot = AuditUsageLedgerSnapshot::default();
        snapshot.record_event(&event(EventCategory::Usage, "ledger.usage.recorded", 7));
        flush(&snapshot, &dir, 0);
        assert_eq!(
            segment_files(&dir),
            vec![segment_file_name(1), segment_file_name(7)]
        );
    }

    #[test]
    fn 最后一段的残行被截断而非活动段残行视为损坏() {
        let dir = temp_dir("torn");
        let mut snapshot = AuditUsageLedgerSnapshot::default();
        snapshot.record_event(&event(EventCategory::Usage, "ledger.usage.recorded", 1));
        flush(&snapshot, &dir, 0);
        let active = dir.join(segment_file_name(1));
        let intact_len = fs::metadata(&active).unwrap().len();
        fs::OpenOptions::new()
            .append(true)
            .open(&active)
            .unwrap()
            .write_all(b"{\"category\":\"usage\",\"event_id\":\"e-2")
            .unwrap();

        let restored = AuditUsageLedgerSnapshot::load_from_dir(&dir, UtcMillis::now()).unwrap();
        assert_eq!(restored.usage_count(), 1);
        assert_eq!(fs::metadata(&active).unwrap().len(), intact_len);

        fs::OpenOptions::new()
            .append(true)
            .open(&active)
            .unwrap()
            .write_all(b"{\"broken\"")
            .unwrap();
        fs::write(dir.join(segment_file_name(9)), "").unwrap();
        assert!(matches!(
            AuditUsageLedgerSnapshot::load_from_dir(&dir, UtcMillis::now()),
            Err(AuditUsageLedgerError::CorruptSegment { .. })
        ));
    }

    #[test]
    fn 完整但无法解析的行拒绝加载() {
        let dir = temp_dir("corrupt");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(segment_file_name(1)), "not-json\n").unwrap();
        assert!(matches!(
            AuditUsageLedgerSnapshot::load_from_dir(&dir, UtcMillis::now()),
            Err(AuditUsageLedgerError::CorruptSegment { .. })
        ));
    }

    #[test]
    fn 过期的旧段被删除但最新一段始终保留() {
        let dir = temp_dir("retention");
        let mut old = AuditUsageLedgerSnapshot::default();
        let mut stale = event(EventCategory::Usage, "ledger.usage.recorded", 1);
        stale.occurred_at = UtcMillis(1_000);
        old.record_event(&stale);
        let old_append = old.append_after(0).unwrap().unwrap();
        AuditUsageLedgerSnapshot::append_to_dir(&dir, &old_append).unwrap();
        let mut newer = AuditUsageLedgerSnapshot::default();
        let mut also_stale = event(EventCategory::Audit, "ledger.audit.recorded", 2);
        also_stale.occurred_at = UtcMillis(2_000);
        newer.record_event(&also_stale);
        let newer_append = newer.append_after(0).unwrap().unwrap();
        fs::write(dir.join(segment_file_name(2)), newer_append.content).unwrap();

        let now = UtcMillis(AUDIT_USAGE_LEDGER_RETENTION_MILLIS + 10_000);
        let restored = AuditUsageLedgerSnapshot::load_from_dir(&dir, now).unwrap();
        assert_eq!(segment_files(&dir), vec![segment_file_name(2)]);
        assert_eq!(restored.usage_count(), 0);
        assert_eq!(restored.audit_count(), 1);
        assert_eq!(restored.next_sequence, 3);
    }
}
