use crate::error::{SnapshotError, SnapshotResult};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const DEBOUNCE_MS: u64 = 200;
const TICK_MS: u64 = 50;

/// 经过 200ms 去抖后向上游发送的事件。
#[derive(Clone, Debug)]
pub struct DebouncedEvent {
    pub path: PathBuf,
    pub kind: DebouncedKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebouncedKind {
    Created,
    Modified,
    Removed,
}

/// 文件系统监控器：notify watcher + tokio 内置 200ms 去抖。
pub struct FsWatcher {
    _watcher: RecommendedWatcher,
    _shutdown_tx: mpsc::Sender<()>,
}

impl FsWatcher {
    pub fn start(
        root: impl AsRef<Path>,
        excluded_prefixes: Arc<Vec<PathBuf>>,
        out: mpsc::UnboundedSender<DebouncedEvent>,
    ) -> SnapshotResult<Self> {
        let root = root.as_ref().to_path_buf();
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<RawEvent>();
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            match res {
                Ok(ev) => {
                    if let Some(raw) = classify(&ev) {
                        for path in ev.paths {
                            let _ = raw_tx.send(RawEvent {
                                path,
                                kind: raw,
                                at: Instant::now(),
                            });
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "notify watcher delivered error");
                }
            }
        })
        .map_err(|e| SnapshotError::Watcher(e.to_string()))?;

        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| SnapshotError::Watcher(e.to_string()))?;

        // 去抖任务：HashMap<path, (kind, last_seen)>，每 50ms 检查一次。
        tokio::spawn(async move {
            let mut pending: HashMap<PathBuf, (DebouncedKind, Instant)> = HashMap::new();
            let mut tick = tokio::time::interval(Duration::from_millis(TICK_MS));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => break,
                    _ = tick.tick() => {
                        let now = Instant::now();
                        let ready: Vec<PathBuf> = pending
                            .iter()
                            .filter_map(|(p, (_, ts))| {
                                if now.duration_since(*ts) >= Duration::from_millis(DEBOUNCE_MS) {
                                    Some(p.clone())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        for p in ready {
                            if let Some((kind, _)) = pending.remove(&p) {
                                if out.send(DebouncedEvent { path: p, kind }).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Some(raw) = raw_rx.recv() => {
                        if path_excluded(&raw.path, &excluded_prefixes) {
                            continue;
                        }
                        let merged = match (pending.get(&raw.path).map(|(k, _)| *k), raw.kind) {
                            // 已有 Created + 后续 Modified → 仍是 Created（首次出现）
                            (Some(DebouncedKind::Created), DebouncedKind::Modified) => DebouncedKind::Created,
                            // 已有 Created + 后续 Removed → 抹掉这条
                            (Some(DebouncedKind::Created), DebouncedKind::Removed) => {
                                pending.remove(&raw.path);
                                continue;
                            }
                            (_, k) => k,
                        };
                        pending.insert(raw.path, (merged, raw.at));
                    }
                }
            }
        });

        Ok(Self {
            _watcher: watcher,
            _shutdown_tx: shutdown_tx,
        })
    }
}

#[derive(Debug)]
struct RawEvent {
    path: PathBuf,
    kind: DebouncedKind,
    at: Instant,
}

fn classify(ev: &Event) -> Option<DebouncedKind> {
    match ev.kind {
        EventKind::Create(_) => Some(DebouncedKind::Created),
        EventKind::Modify(_) => Some(DebouncedKind::Modified),
        EventKind::Remove(_) => Some(DebouncedKind::Removed),
        _ => None,
    }
}

fn path_excluded(path: &Path, prefixes: &[PathBuf]) -> bool {
    prefixes.iter().any(|p| path.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excluded_prefix_matches() {
        let prefixes = vec![PathBuf::from("/a/b/.magi")];
        assert!(path_excluded(Path::new("/a/b/.magi/x"), &prefixes));
        assert!(!path_excluded(Path::new("/a/b/src/x"), &prefixes));
    }
}
