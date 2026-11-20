use super::{CanonicalEventTransaction, DaemonError, event_identity_matches};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use magi_core::SessionId;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
};

const HISTORY_DIR: &str = "history";
const HISTORY_SUFFIX: &str = ".json.gz";

/// A checkpoint references immutable, gap-free ranges of original transactions.
/// The first range may start after a checkpoint written before retention existed.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct HistorySegment {
    pub(super) first_event_seq: u64,
    pub(super) last_event_seq: u64,
}

impl HistorySegment {
    pub(super) fn new(first_event_seq: u64, last_event_seq: u64) -> Result<Self, DaemonError> {
        if first_event_seq == 0 || first_event_seq > last_event_seq {
            return Err(DaemonError::internal("会话历史分段的范围不合法"));
        }
        Ok(Self {
            first_event_seq,
            last_event_seq,
        })
    }

    pub(super) fn path(self, event_root: &Path) -> PathBuf {
        event_root.join(HISTORY_DIR).join(format!(
            "{:020}-{:020}{HISTORY_SUFFIX}",
            self.first_event_seq, self.last_event_seq
        ))
    }

    pub(super) fn contains(self, event_seq: u64) -> bool {
        (self.first_event_seq..=self.last_event_seq).contains(&event_seq)
    }

    pub(super) fn archive(
        self,
        event_root: &Path,
        transactions: &[(Self, PathBuf)],
    ) -> Result<(), DaemonError> {
        let directory = event_root.join(HISTORY_DIR);
        fs::create_dir_all(&directory)?;
        magi_core::fs_atomic::write_atomic_with(&self.path(event_root), |file| {
            let mut encoder = GzEncoder::new(file, Compression::fast());
            let mut cursor = self.first_event_seq - 1;
            for (range, path) in transactions.iter().filter(|(range, _)| {
                (self.first_event_seq..=self.last_event_seq).contains(&range.last_event_seq)
            }) {
                if range.first_event_seq != cursor + 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "会话历史封存的事务不连续",
                    ));
                }
                if io::copy(&mut fs::File::open(path)?, &mut encoder)? == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "会话历史封存的事务为空",
                    ));
                }
                cursor = range.last_event_seq;
                encoder.write_all(b"\n")?;
            }
            if cursor != self.last_event_seq {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "会话历史封存缺少事务",
                ));
            }
            encoder.finish()?;
            Ok(())
        })?;
        // The directory itself can have been created for this first segment.
        // Persist its entry before the checkpoint is allowed to reference it.
        if let Ok(directory) = fs::File::open(event_root) {
            let _ = directory.sync_all();
        }
        Ok(())
    }

    /// A crash after compression and before checkpoint publication can leave a
    /// segment with the same first sequence and an earlier end. Keep only the
    /// range named by the committed checkpoint.
    pub(super) fn remove_failed_attempts(self, event_root: &Path) {
        let prefix = format!("{:020}-", self.first_event_seq);
        let committed = self.path(event_root);
        let Ok(entries) = fs::read_dir(event_root.join(HISTORY_DIR)) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let is_attempt = name.to_str().is_some_and(|name| {
                name.strip_prefix(&prefix)
                    .and_then(|name| name.strip_suffix(HISTORY_SUFFIX))
                    .is_some_and(|end| end.len() == 20 && end.parse::<u64>().is_ok())
            });
            if is_attempt
                && entry.path() != committed
                && let Err(error) = fs::remove_file(entry.path())
            {
                tracing::warn!(%error, path = %entry.path().display(), "清理未接纳的会话历史封存失败");
            }
        }
    }
}

pub(super) fn validate_index(
    segments: &[HistorySegment],
    checkpoint_seq: u64,
) -> Result<(), DaemonError> {
    if segments.is_empty() {
        return Ok(());
    }
    let mut next = segments[0].first_event_seq;
    for segment in segments {
        HistorySegment::new(segment.first_event_seq, segment.last_event_seq)?;
        if segment.first_event_seq != next {
            return Err(DaemonError::internal("会话历史封存序列不连续"));
        }
        next = segment.last_event_seq.saturating_add(1);
    }
    if segments
        .last()
        .is_some_and(|segment| segment.last_event_seq != checkpoint_seq)
    {
        return Err(DaemonError::internal("会话历史封存与检查点水位不一致"));
    }
    Ok(())
}

/// A read snapshot owns an open file, so checkpoint rotation cannot switch
/// sources or remove this page while the reader is using it.
pub(in crate::daemon) struct EventHistoryRead {
    after: u64,
    pub(in crate::daemon) through: u64,
    pub(in crate::daemon) first_retained: u64,
    source: Option<(HistorySegment, fs::File, bool)>,
}

impl EventHistoryRead {
    pub(super) fn new(after: u64, through: u64, first_retained: u64) -> Self {
        Self {
            after,
            through,
            first_retained,
            source: None,
        }
    }

    pub(super) fn next_sequence(&self) -> Option<u64> {
        if self.after >= self.through {
            return None;
        }
        let next = (self.after + 1).max(self.first_retained);
        (next <= self.through).then_some(next)
    }

    pub(super) fn open(
        &mut self,
        range: HistorySegment,
        path: &Path,
        compressed: bool,
    ) -> Result<(), DaemonError> {
        self.source = Some((range, fs::File::open(path)?, compressed));
        Ok(())
    }

    pub(super) fn transactions(
        self,
    ) -> Result<
        (
            HistorySegment,
            impl Iterator<Item = Result<CanonicalEventTransaction, serde_json::Error>>,
        ),
        DaemonError,
    > {
        let (range, file, compressed) = self
            .source
            .ok_or_else(|| DaemonError::internal("未打开会话历史事务"))?;
        let source: Box<dyn Read> = if compressed {
            Box::new(GzDecoder::new(file))
        } else {
            Box::new(file)
        };
        Ok((
            range,
            serde_json::Deserializer::from_reader(BufReader::new(source)).into_iter(),
        ))
    }
}

pub(super) fn validate_history_transaction(
    transaction: &CanonicalEventTransaction,
    session_id: &SessionId,
    first: u64,
    end: u64,
) -> Result<(), DaemonError> {
    let valid =
        transaction.matches_header(session_id, first)
            && transaction.last_event_seq <= end
            && transaction.events.iter().enumerate().all(|(index, event)| {
                event_identity_matches(event, session_id, first + index as u64)
            });
    if valid {
        Ok(())
    } else {
        Err(DaemonError::internal("会话历史事务的身份或顺序不合法"))
    }
}
