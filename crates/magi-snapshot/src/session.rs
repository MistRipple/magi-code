use crate::baseline_index::{BaselineIndex, RefsIndex, baseline_path, refs_path};
use crate::blob_store::BlobStore;
use crate::change_log::ChangeLog;
use crate::error::{SnapshotError, SnapshotResult};
use crate::scan::{SnapshotPathFilter, read_file_meta, read_large_text_summary, walk_workspace};
use crate::tool_hook::{ToolHook, ToolHookCtx};
use crate::types::{
    ChangeEvent, ChangeKind, ContentKind, FileMeta, PendingChange, SourceKind, SymlinkTargetKind,
};
use crate::watcher::{DebouncedEvent, DebouncedKind, FsWatcher};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

/// 单个 session 的快照账本。线程安全，可被 AppState 共享。
pub struct SnapshotSession {
    session_id: String,
    workspace_root: PathBuf,
    session_dir: PathBuf,
    blobs: Arc<BlobStore>,
    baseline: RwLock<BaselineIndex>,
    refs: RwLock<RefsIndex>,
    events: Arc<ChangeLog>,
    /// 跟踪「当前 session 看到的最新文件状态」。projection 时与 baseline 对比。
    current: RwLock<HashMap<String, FileMeta>>,
    /// 路径 → 最近一次写入事件的归因（source / tool_call_id / worker_id）。
    last_event: RwLock<HashMap<String, ChangeEvent>>,
    /// 当前正在执行中的工具调用。串行执行时 watcher 事件可直接归因；并发执行时仅按声明路径精确归因。
    active_tool_ctxs: RwLock<HashMap<String, ToolHookCtx>>,
    path_filter: SnapshotPathFilter,
    _watcher: tokio::sync::Mutex<Option<FsWatcher>>,
}

impl SnapshotSession {
    /// 启动一个新 session。立即构建 baseline 并启动 watcher。
    pub async fn start(
        session_id: String,
        workspace_root: PathBuf,
        blobs: Arc<BlobStore>,
        snapshots_root: PathBuf,
        respect_gitignore: bool,
    ) -> SnapshotResult<Arc<Self>> {
        if !workspace_root.is_absolute() {
            return Err(SnapshotError::InvalidRoot(format!(
                "workspace_root must be absolute: {}",
                workspace_root.display()
            )));
        }
        if !workspace_root.is_dir() {
            return Err(SnapshotError::InvalidRoot(format!(
                "workspace_root not a directory: {}",
                workspace_root.display()
            )));
        }
        // notify / fsevents 在 macOS 上返回 canonical 路径（/private/var/...），
        // 这里统一规范化以便后续 starts_with 比较一致。
        let workspace_root = std::fs::canonicalize(&workspace_root)
            .map_err(|e| SnapshotError::io(&workspace_root, e))?;

        let session_dir = snapshots_root.join("index").join(&session_id);
        std::fs::create_dir_all(&session_dir).map_err(|e| SnapshotError::io(&session_dir, e))?;
        let path_filter = SnapshotPathFilter::new(&workspace_root, respect_gitignore);

        let baseline = BaselineIndex::load(&baseline_path(&session_dir))?;
        let refs = RefsIndex::load(&refs_path(&session_dir))?;
        let events = Arc::new(ChangeLog::open(session_dir.join("events.log"))?);

        let session = Arc::new(Self {
            session_id,
            workspace_root: workspace_root.clone(),
            session_dir,
            blobs: blobs.clone(),
            baseline: RwLock::new(baseline),
            refs: RwLock::new(refs),
            events: events.clone(),
            current: RwLock::new(HashMap::new()),
            last_event: RwLock::new(HashMap::new()),
            active_tool_ctxs: RwLock::new(HashMap::new()),
            path_filter,
            _watcher: tokio::sync::Mutex::new(None),
        });

        // baseline 不存在时（首次启动），后台扫描并填充。
        let needs_initial_scan = session
            .baseline
            .read()
            .expect("baseline poisoned")
            .is_empty();
        if needs_initial_scan {
            session.run_initial_scan(respect_gitignore)?;
        } else {
            session.replay_events_into_current()?;
        }

        // 启动 watcher。
        let (tx, mut rx) = mpsc::unbounded_channel::<DebouncedEvent>();
        let excluded = Arc::new(session.path_filter.excluded_prefixes());
        let watcher = FsWatcher::start(&workspace_root, excluded, tx)?;
        {
            let mut guard = session._watcher.lock().await;
            *guard = Some(watcher);
        }

        let weak = Arc::downgrade(&session);
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                let Some(s) = weak.upgrade() else { break };
                let _ = s.handle_watcher_event(ev);
            }
        });

        Ok(session)
    }

    /// 关闭 watcher，停止接收新事件。事件日志与 baseline 保留。
    pub async fn archive(&self) {
        let mut guard = self._watcher.lock().await;
        *guard = None;
    }

    /// 删除 session：停 watcher、清 baseline/events/refs、释放 blob 引用。
    pub async fn drop_session(&self) -> SnapshotResult<()> {
        self.archive().await;
        let baseline = self.baseline.read().expect("baseline poisoned");
        for meta in baseline.entries.values() {
            if let Some(h) = &meta.blob_hash {
                self.blobs.release(h)?;
            }
        }
        drop(baseline);
        if self.session_dir.exists() {
            std::fs::remove_dir_all(&self.session_dir)
                .map_err(|e| SnapshotError::io(&self.session_dir, e))?;
        }
        Ok(())
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    fn run_initial_scan(&self, respect_gitignore: bool) -> SnapshotResult<()> {
        let paths = walk_workspace(&self.workspace_root, respect_gitignore)?;
        let mut baseline = BaselineIndex::default();
        for abs in paths {
            match read_file_meta(&self.workspace_root, &abs, &self.blobs) {
                Ok(meta) => baseline.upsert(meta),
                Err(err) => {
                    tracing::warn!(path = %abs.display(), error = %err, "baseline scan failed for path");
                }
            }
        }
        baseline.save(&baseline_path(&self.session_dir))?;
        let mut current = self.current.write().expect("current poisoned");
        *current = baseline.entries.clone().into_iter().collect();
        let mut guard = self.baseline.write().expect("baseline poisoned");
        *guard = baseline;
        Ok(())
    }

    fn replay_events_into_current(&self) -> SnapshotResult<()> {
        let baseline = self.baseline.read().expect("baseline poisoned");
        let mut current: HashMap<String, FileMeta> = baseline.entries.clone().into_iter().collect();
        drop(baseline);

        let events = self.events.read_all()?;
        let mut last_event: HashMap<String, ChangeEvent> = HashMap::new();
        for ev in events {
            let path_key = ev
                .after
                .as_ref()
                .map(|m| m.path.clone())
                .or_else(|| ev.before.as_ref().map(|m| m.path.clone()));
            if let Some(p) = path_key {
                match ev.change_kind {
                    ChangeKind::Deleted => {
                        current.remove(&p);
                    }
                    _ => {
                        if let Some(after) = &ev.after {
                            current.insert(p.clone(), after.clone());
                        }
                    }
                }
                last_event.insert(p, ev);
            }
        }

        *self.current.write().expect("current poisoned") = current;
        *self.last_event.write().expect("last_event poisoned") = last_event;
        Ok(())
    }

    fn handle_watcher_event(&self, ev: DebouncedEvent) -> SnapshotResult<()> {
        let abs = ev.path;
        if !abs.starts_with(&self.workspace_root) {
            return Ok(());
        }
        if self.path_filter.excludes_abs_path(&abs) {
            return Ok(());
        }
        let source = SourceKind::Watcher;
        let ctx = self.active_tool_context_for_path(&abs);
        match ev.kind {
            DebouncedKind::Removed => self.record_removal(&abs, source, ctx)?,
            DebouncedKind::Created | DebouncedKind::Modified => {
                self.record_upsert(&abs, source, ctx)?
            }
        }
        Ok(())
    }

    fn record_upsert(
        &self,
        abs: &Path,
        source: SourceKind,
        ctx: Option<ToolHookCtx>,
    ) -> SnapshotResult<()> {
        if std::fs::symlink_metadata(abs).is_err() {
            return self.record_removal(abs, source, ctx);
        }
        let meta = read_file_meta(&self.workspace_root, abs, &self.blobs)?;
        let path_key = meta.path.clone();
        let before = self
            .current
            .read()
            .expect("current poisoned")
            .get(&path_key)
            .cloned();

        let unchanged = before
            .as_ref()
            .map(|b| meta_unchanged(b, &meta))
            .unwrap_or(false);
        if unchanged {
            return Ok(());
        }

        let change_kind = if before.is_some() {
            ChangeKind::Modified
        } else {
            ChangeKind::Added
        };

        let event = ChangeEvent {
            event_id: new_event_id(),
            timestamp_ms: now_ms(),
            change_kind,
            source: ctx.as_ref().map(|_| SourceKind::Tool).unwrap_or(source),
            tool_call_id: ctx.as_ref().map(|c| c.tool_call_id.clone()),
            worker_id: ctx.as_ref().and_then(|c| c.worker_id.clone()),
            execution_group_id: ctx.as_ref().and_then(|c| c.execution_group_id.clone()),
            before,
            after: Some(meta.clone()),
        };

        self.events.append(&event)?;
        self.current
            .write()
            .expect("current poisoned")
            .insert(path_key.clone(), meta);
        self.last_event
            .write()
            .expect("last_event poisoned")
            .insert(path_key, event);
        Ok(())
    }

    fn record_removal(
        &self,
        abs: &Path,
        source: SourceKind,
        ctx: Option<ToolHookCtx>,
    ) -> SnapshotResult<()> {
        let rel = match abs.strip_prefix(&self.workspace_root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => return Ok(()),
        };
        let before = self.current.write().expect("current poisoned").remove(&rel);
        let event = ChangeEvent {
            event_id: new_event_id(),
            timestamp_ms: now_ms(),
            change_kind: ChangeKind::Deleted,
            source: ctx.as_ref().map(|_| SourceKind::Tool).unwrap_or(source),
            tool_call_id: ctx.as_ref().map(|c| c.tool_call_id.clone()),
            worker_id: ctx.as_ref().and_then(|c| c.worker_id.clone()),
            execution_group_id: ctx.as_ref().and_then(|c| c.execution_group_id.clone()),
            before,
            after: None,
        };
        self.events.append(&event)?;
        self.last_event
            .write()
            .expect("last_event poisoned")
            .insert(rel, event);
        Ok(())
    }

    /// 全树对账：在工具执行批次结束后调用，兜住 watcher 漏掉的事件。
    pub fn reconcile(&self) -> SnapshotResult<()> {
        let respect = self.workspace_root.join(".git").is_dir();
        let paths = walk_workspace(&self.workspace_root, respect)?;
        let mut seen = std::collections::HashSet::new();
        for abs in paths {
            let rel = match abs.strip_prefix(&self.workspace_root) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            if self.path_filter.excludes_relative_str(&rel) {
                continue;
            }
            seen.insert(rel.clone());
            self.record_upsert(
                &abs,
                SourceKind::External,
                self.active_tool_context_for_path(&abs),
            )?;
        }

        // 处理删除：当前集合 vs seen 集合
        let known_paths: Vec<String> = self
            .current
            .read()
            .expect("current poisoned")
            .keys()
            .cloned()
            .collect();
        for k in known_paths {
            if self.path_filter.excludes_relative_str(&k) {
                continue;
            }
            if !seen.contains(&k) {
                let abs = self.workspace_root.join(&k);
                self.record_removal(
                    &abs,
                    SourceKind::External,
                    self.active_tool_context_for_path(&abs),
                )?;
            }
        }
        Ok(())
    }

    fn active_tool_context_for_path(&self, abs: &Path) -> Option<ToolHookCtx> {
        let rel = abs
            .strip_prefix(&self.workspace_root)
            .ok()
            .map(normalized_path)?;
        let active = self.active_tool_ctxs.read().expect("ctx poisoned");
        if active.is_empty() {
            return None;
        }
        if active.len() == 1 {
            return active.values().next().cloned();
        }

        let mut matched = active
            .values()
            .filter(|ctx| {
                ctx.declared_paths
                    .iter()
                    .any(|path| declared_path_matches(&self.workspace_root, path, abs, &rel))
            })
            .cloned();
        let first = matched.next()?;
        if matched.next().is_some() {
            None
        } else {
            Some(first)
        }
    }

    /// 投影出当前的 pending changes。
    ///
    /// 基础策略是 baseline vs current 的差集（add / modify / delete）；若同一 blob_hash 同时
    /// 出现在一条 `Deleted` 与一条 `Added` 中，则配对折叠成 `Renamed`，并由 `old_path` 指回
    /// 原 baseline 路径，确保 rename 不再被前端误解为删除 + 新增。
    pub fn pending_changes(&self) -> SnapshotResult<Vec<PendingChange>> {
        let baseline = self.baseline.read().expect("baseline poisoned");
        let current = self.current.read().expect("current poisoned");
        let last_event = self.last_event.read().expect("last_event poisoned");

        let mut all_paths: std::collections::BTreeSet<String> = baseline
            .entries
            .keys()
            .cloned()
            .chain(current.keys().cloned())
            .collect();
        all_paths.extend(last_event.keys().cloned());

        let mut primary = Vec::new();
        for p in all_paths {
            if self.path_filter.excludes_relative_str(&p) {
                continue;
            }
            let base = baseline.entries.get(&p);
            let now = current.get(&p);
            let pending = match (base, now) {
                (None, None) => continue,
                (Some(b), Some(n)) if meta_unchanged(b, n) => continue,
                (Some(b), Some(n)) => self.project(p.clone(), Some(b), Some(n), &last_event)?,
                (None, Some(n)) => self.project(p.clone(), None, Some(n), &last_event)?,
                (Some(b), None) => self.project(p.clone(), Some(b), None, &last_event)?,
            };
            if let Some(pc) = pending {
                primary.push(pc);
            }
        }

        let mut out = collapse_renames(primary, &baseline.entries, &current);
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn project(
        &self,
        path: String,
        base: Option<&FileMeta>,
        now: Option<&FileMeta>,
        last_event: &HashMap<String, ChangeEvent>,
    ) -> SnapshotResult<Option<PendingChange>> {
        let change_kind = match (base, now) {
            (Some(_), Some(_)) => ChangeKind::Modified,
            (None, Some(_)) => ChangeKind::Added,
            (Some(_), None) => ChangeKind::Deleted,
            (None, None) => return Ok(None),
        };

        let event = last_event.get(&path);
        let source = event.map(|e| e.source).unwrap_or(SourceKind::External);
        let tool_call_id = event.and_then(|e| e.tool_call_id.clone());
        let worker_id = event.and_then(|e| e.worker_id.clone());
        let execution_group_id = event.and_then(|e| e.execution_group_id.clone());
        let timestamp_ms = event.map(|e| e.timestamp_ms).unwrap_or_else(now_ms);

        let primary_meta = now.or(base).expect("at least one side present");
        let content_kind = primary_meta.content_kind;
        let size = primary_meta.size;
        let mime = primary_meta.mime.clone();
        let error = primary_meta.error.clone();
        let symlink_target = primary_meta.symlink.as_ref().map(|s| s.target.clone());

        let mut original_content: Option<String> = None;
        let mut preview_content: Option<String> = None;
        let mut head_summary: Option<String> = None;
        let mut tail_summary: Option<String> = None;
        let mut unified_diff: Option<String> = None;

        if matches!(content_kind, ContentKind::Text) {
            if let Some(b) = base {
                if let Some(h) = &b.blob_hash {
                    if let Ok(bytes) = self.blobs.get(h, true) {
                        original_content = Some(String::from_utf8_lossy(&bytes).into_owned());
                    }
                }
            }
            if change_kind != ChangeKind::Deleted {
                let abs = self.workspace_root.join(&path);
                if let Ok(bytes) = std::fs::read(&abs) {
                    preview_content = Some(String::from_utf8_lossy(&bytes).into_owned());
                }
            }
            unified_diff = match (&original_content, &preview_content) {
                (Some(o), Some(n)) => Some(unified_diff_text(&path, o, n)),
                (Some(o), None) => Some(unified_diff_text(&path, o, "")),
                (None, Some(n)) => Some(unified_diff_text(&path, "", n)),
                (None, None) => None,
            };
        } else if matches!(content_kind, ContentKind::LargeText)
            && change_kind != ChangeKind::Deleted
        {
            let abs = self.workspace_root.join(&path);
            let (h, t) = read_large_text_summary(&abs);
            head_summary = h;
            tail_summary = t;
        }

        Ok(Some(PendingChange {
            path,
            change_kind,
            old_path: None,
            source,
            tool_call_id,
            worker_id,
            execution_group_id,
            content_kind,
            size,
            mime,
            error,
            symlink_target,
            original_content,
            preview_content,
            head_summary,
            tail_summary,
            unified_diff,
            timestamp_ms,
        }))
    }

    /// 把当前 path 的状态推进到 baseline。删除事件则同时从 baseline 移除。
    pub fn approve(&self, paths: &[String]) -> SnapshotResult<usize> {
        let paths = self.expand_rename_pairs(paths)?;
        let mut baseline = self.baseline.write().expect("baseline poisoned");
        let mut refs = self.refs.write().expect("refs poisoned");
        let current = self.current.read().expect("current poisoned");
        let mut applied = 0usize;

        for p in &paths {
            match current.get(p) {
                Some(meta) => {
                    if let Some(old) = baseline.entries.get(p) {
                        if let Some(h) = &old.blob_hash {
                            self.blobs.release(h)?;
                        }
                    }
                    if let Some(h) = &meta.blob_hash {
                        self.blobs.retain(h, 1);
                    }
                    baseline.upsert(meta.clone());
                    refs.upsert(meta.clone());
                    applied += 1;
                }
                None => {
                    if let Some(old) = baseline.remove(p) {
                        if let Some(h) = &old.blob_hash {
                            self.blobs.release(h)?;
                        }
                        applied += 1;
                    }
                }
            }
        }

        baseline.save(&baseline_path(&self.session_dir))?;
        refs.save(&refs_path(&self.session_dir))?;

        // 清掉 last_event 里这些 path 的归因（已不再 pending）。
        let mut last_event = self.last_event.write().expect("last_event poisoned");
        for p in &paths {
            last_event.remove(p);
        }
        Ok(applied)
    }

    /// 把 paths 还原到 baseline 状态。
    pub fn revert(&self, paths: &[String]) -> SnapshotResult<usize> {
        let paths = self.expand_rename_pairs(paths)?;
        let baseline = self.baseline.read().expect("baseline poisoned");
        let mut applied = 0usize;
        for p in &paths {
            let abs = self.workspace_root.join(p);
            match baseline.entries.get(p) {
                Some(meta) => match meta.content_kind {
                    ContentKind::Text | ContentKind::Binary => {
                        let h = meta.blob_hash.as_ref().ok_or_else(|| {
                            SnapshotError::Internal(format!("baseline blob missing for {p}"))
                        })?;
                        let compressed = matches!(meta.content_kind, ContentKind::Text);
                        let bytes = self.blobs.get(h, compressed)?;
                        if let Some(parent) = abs.parent() {
                            std::fs::create_dir_all(parent)
                                .map_err(|e| SnapshotError::io(parent, e))?;
                        }
                        std::fs::write(&abs, &bytes).map_err(|e| SnapshotError::io(&abs, e))?;
                        applied += 1;
                    }
                    ContentKind::Symlink => {
                        let target = meta.symlink.as_ref().ok_or_else(|| {
                            SnapshotError::Internal(format!(
                                "baseline symlink target missing for {p}"
                            ))
                        })?;
                        if let Some(parent) = abs.parent() {
                            std::fs::create_dir_all(parent)
                                .map_err(|e| SnapshotError::io(parent, e))?;
                        }
                        if std::fs::symlink_metadata(&abs).is_ok() {
                            remove_file_or_symlink(&abs).map_err(|e| SnapshotError::io(&abs, e))?;
                        }
                        restore_symlink(&target.target, target.target_kind, &abs)
                            .map_err(|e| SnapshotError::io(&abs, e))?;
                        applied += 1;
                    }
                    ContentKind::LargeText | ContentKind::Special => {
                        return Err(SnapshotError::Internal(format!(
                            "baseline content kind {:?} for {p} cannot be restored without a blob",
                            meta.content_kind
                        )));
                    }
                },
                None => {
                    if std::fs::symlink_metadata(&abs).is_ok() {
                        remove_file_or_symlink(&abs).map_err(|e| SnapshotError::io(&abs, e))?;
                        applied += 1;
                    }
                }
            }
            if std::fs::symlink_metadata(&abs).is_ok() {
                self.record_upsert(&abs, SourceKind::Tool, None)?;
            } else {
                self.record_removal(&abs, SourceKind::Tool, None)?;
            }
        }
        Ok(applied)
    }

    /// 如果 `paths` 命中了某个 rename 对的一端（新路径或旧路径），把另一端也纳入。
    ///
    /// rename 在 projection 层被折叠为单行 `Renamed`，但底层 baseline/current 仍是 old/new 双端。
    /// approve/revert 必须同时处理两端，否则：
    /// - approve 会漏删 baseline 中的旧路径条目；
    /// - revert 会漏恢复旧路径文件或漏删新路径文件。
    fn expand_rename_pairs(&self, paths: &[String]) -> SnapshotResult<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let pending = self.pending_changes()?;
        let rename_lookup: HashMap<&str, &str> = pending
            .iter()
            .filter(|c| c.change_kind == ChangeKind::Renamed)
            .filter_map(|c| c.old_path.as_deref().map(|old| (c.path.as_str(), old)))
            .collect();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<String> = Vec::with_capacity(paths.len());
        let push =
            |p: String, seen: &mut std::collections::HashSet<String>, out: &mut Vec<String>| {
                if seen.insert(p.clone()) {
                    out.push(p);
                }
            };

        for p in paths {
            push(p.clone(), &mut seen, &mut out);
            if let Some(old) = rename_lookup.get(p.as_str()) {
                push((*old).to_string(), &mut seen, &mut out);
                continue;
            }
            if let Some((new_path, _)) = rename_lookup.iter().find(|(_, old)| **old == p.as_str()) {
                push((*new_path).to_string(), &mut seen, &mut out);
            }
        }
        Ok(out)
    }
}

impl ToolHook for SnapshotSession {
    fn before_tool(&self, ctx: &ToolHookCtx) {
        self.active_tool_ctxs
            .write()
            .expect("ctx poisoned")
            .insert(ctx.tool_call_id.clone(), ctx.clone());
    }

    fn after_tool(&self, ctx: &ToolHookCtx) {
        for p in &ctx.declared_paths {
            let abs = if p.is_absolute() {
                p.clone()
            } else {
                self.workspace_root.join(p)
            };
            if std::fs::symlink_metadata(&abs).is_ok() {
                let _ = self.record_upsert(&abs, SourceKind::Tool, Some(ctx.clone()));
            } else {
                let _ = self.record_removal(&abs, SourceKind::Tool, Some(ctx.clone()));
            }
        }
        self.active_tool_ctxs
            .write()
            .expect("ctx poisoned")
            .remove(&ctx.tool_call_id);
    }
}

fn collapse_renames(
    pending: Vec<PendingChange>,
    baseline: &BTreeMap<String, FileMeta>,
    current: &HashMap<String, FileMeta>,
) -> Vec<PendingChange> {
    use std::collections::HashMap;

    let mut deleted_by_hash: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, change) in pending.iter().enumerate() {
        if change.change_kind != ChangeKind::Deleted {
            continue;
        }
        let Some(meta) = baseline.get(&change.path) else {
            continue;
        };
        if !matches!(meta.content_kind, ContentKind::Text | ContentKind::Binary) {
            continue;
        }
        let Some(hash) = meta.blob_hash.as_ref() else {
            continue;
        };
        deleted_by_hash.entry(hash.clone()).or_default().push(idx);
    }

    let mut pending: Vec<Option<PendingChange>> = pending.into_iter().map(Some).collect();

    for added_index in 0..pending.len() {
        let Some(added) = pending[added_index].as_ref() else {
            continue;
        };
        if added.change_kind != ChangeKind::Added {
            continue;
        }
        if !matches!(added.content_kind, ContentKind::Text | ContentKind::Binary) {
            continue;
        }
        let Some(hash) = current
            .get(&added.path)
            .and_then(|meta| meta.blob_hash.clone())
        else {
            continue;
        };
        let Some(candidates) = deleted_by_hash.get_mut(&hash) else {
            continue;
        };
        let Some(deleted_index) = candidates.pop() else {
            continue;
        };
        if candidates.is_empty() {
            deleted_by_hash.remove(&hash);
        }
        let Some(deleted) = pending[deleted_index].take() else {
            continue;
        };
        if let Some(renamed) = pending[added_index].as_mut() {
            renamed.change_kind = ChangeKind::Renamed;
            renamed.old_path = Some(deleted.path);
        }
    }

    pending.into_iter().flatten().collect()
}

fn declared_path_matches(workspace_root: &Path, declared: &Path, abs: &Path, rel: &str) -> bool {
    if declared.is_absolute() {
        return declared == abs;
    }
    normalized_path(declared) == rel || workspace_root.join(declared) == abs
}

fn normalized_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

fn meta_unchanged(a: &FileMeta, b: &FileMeta) -> bool {
    a.path == b.path
        && a.content_kind == b.content_kind
        && a.size == b.size
        && a.blob_hash == b.blob_hash
        && a.error == b.error
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn new_event_id() -> String {
    format!("ev-{:016x}", now_ns_xor())
}

fn now_ns_xor() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// 生成 unified diff 文本。
///
/// 约束：输出必须符合 unified diff 标准——文件头 `--- a/path` / `+++ b/path`
/// 之后必须有 `@@ -a,b +c,d @@` hunk header，否则前端按标准格式解析时会丢弃所有 +/- 行。
///
/// 实现说明：当前用"按行索引一一对齐"的简化算法（非最优 LCS），
/// 整个文件作为单一 hunk 输出。语义最优性可在后续单独优化（引入 similar crate 等），
/// 此处目标是保证**格式合法**——hunk header 必须存在，行数必须与 body 一致。
fn unified_diff_text(path: &str, old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut body: Vec<String> = Vec::with_capacity(old_lines.len() + new_lines.len());
    let max_len = old_lines.len().max(new_lines.len());
    for i in 0..max_len {
        match (old_lines.get(i), new_lines.get(i)) {
            (Some(o), Some(n)) if o == n => body.push(format!(" {o}")),
            (Some(o), Some(n)) => {
                body.push(format!("-{o}"));
                body.push(format!("+{n}"));
            }
            (Some(o), None) => body.push(format!("-{o}")),
            (None, Some(n)) => body.push(format!("+{n}")),
            (None, None) => {}
        }
    }

    // hunk header：unified diff 标准要求行数为 0 时起始行也为 0
    let (old_start, old_count) = if old_lines.is_empty() {
        (0, 0)
    } else {
        (1, old_lines.len())
    };
    let (new_start, new_count) = if new_lines.is_empty() {
        (0, 0)
    } else {
        (1, new_lines.len())
    };

    let mut output: Vec<String> = Vec::with_capacity(body.len() + 3);
    output.push(format!("--- a/{path}"));
    output.push(format!("+++ b/{path}"));
    output.push(format!(
        "@@ -{old_start},{old_count} +{new_start},{new_count} @@"
    ));
    output.extend(body);
    output.join("\n")
}

fn remove_file_or_symlink(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(file_error) => {
            #[cfg(windows)]
            {
                if std::fs::symlink_metadata(path)
                    .map(|meta| meta.file_type().is_symlink())
                    .unwrap_or(false)
                {
                    return std::fs::remove_dir(path).map_err(|_| file_error);
                }
            }
            Err(file_error)
        }
    }
}

#[cfg(unix)]
fn restore_symlink(
    target: &str,
    _target_kind: SymlinkTargetKind,
    link_path: &Path,
) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link_path)
}

#[cfg(windows)]
fn restore_symlink(
    target: &str,
    target_kind: SymlinkTargetKind,
    link_path: &Path,
) -> std::io::Result<()> {
    let target_path = Path::new(target);
    let resolved_kind = match target_kind {
        SymlinkTargetKind::File | SymlinkTargetKind::Directory => target_kind,
        SymlinkTargetKind::Unknown => infer_existing_symlink_target_kind(target_path, link_path),
    };

    match resolved_kind {
        SymlinkTargetKind::Directory => std::os::windows::fs::symlink_dir(target_path, link_path),
        SymlinkTargetKind::File | SymlinkTargetKind::Unknown => {
            std::os::windows::fs::symlink_file(target_path, link_path)
        }
    }
}

#[cfg(windows)]
fn infer_existing_symlink_target_kind(target: &Path, link_path: &Path) -> SymlinkTargetKind {
    let resolved_target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(target)
    };

    match std::fs::metadata(resolved_target) {
        Ok(meta) if meta.is_dir() => SymlinkTargetKind::Directory,
        Ok(meta) if meta.is_file() => SymlinkTargetKind::File,
        Ok(_) | Err(_) => SymlinkTargetKind::Unknown,
    }
}

#[cfg(test)]
mod unified_diff_text_tests {
    use super::unified_diff_text;

    /// 契约：输出必须包含 `@@ ... @@` hunk header；前端 RightPane 解析器
    /// 依赖此 header 才会创建 hunk，否则所有 +/- 行被静默丢弃。
    #[test]
    fn add_file_emits_hunk_header_and_plus_lines() {
        let out = unified_diff_text("foo.txt", "", "a\nb\n");
        assert!(out.contains("--- a/foo.txt"));
        assert!(out.contains("+++ b/foo.txt"));
        assert!(
            out.contains("@@ -0,0 +1,2 @@"),
            "missing hunk header in:\n{out}"
        );
        assert!(out.contains("+a"));
        assert!(out.contains("+b"));
    }

    #[test]
    fn delete_file_emits_hunk_header_and_minus_lines() {
        let out = unified_diff_text("foo.txt", "x\ny\n", "");
        assert!(
            out.contains("@@ -1,2 +0,0 @@"),
            "missing hunk header in:\n{out}"
        );
        assert!(out.contains("-x"));
        assert!(out.contains("-y"));
    }

    #[test]
    fn modify_file_emits_hunk_header_with_both_sides() {
        let out = unified_diff_text("foo.txt", "a\nb\n", "a\nc\n");
        assert!(
            out.contains("@@ -1,2 +1,2 @@"),
            "missing hunk header in:\n{out}"
        );
        assert!(out.contains(" a"));
        assert!(out.contains("-b"));
        assert!(out.contains("+c"));
    }
}
