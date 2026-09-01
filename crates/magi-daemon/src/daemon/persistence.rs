use super::config::DaemonError;
use super::session_event_log::SessionConversationProjection;
#[cfg(test)]
use magi_core::TaskId;
use magi_core::{DomainError, DomainResult, SessionId, Task};
use magi_event_bus::AuditUsageLedgerSnapshot;
use magi_knowledge_store::KnowledgeState;
use magi_orchestrator::task_store::{TaskStore, TaskStoreSnapshot};
use magi_session_store::{
    CanonicalTurnEventWriter, CanonicalTurnMutation, SessionAcceptanceRecord, SessionDurableState,
    SessionExecutionSidecarStoreState, SessionRuntimeSidecar, SessionStore,
};
use magi_worker_runtime::{WorkerRuntime, WorkerRuntimeDurableSnapshot};
use magi_workspace::{WorkspaceDurableState, WorkspaceRecoverySidecarStoreState, WorkspaceStore};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tracing::warn;

const ACCEPTED_SUBMISSION_JOURNAL_SCHEMA_VERSION: u32 = 2;
const SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub(crate) struct StateRepository {
    state_root: PathBuf,
    write_lock: Arc<Mutex<()>>,
    session_projection_cache: Arc<Mutex<SessionProjectionCache>>,
    session_event_cache: Arc<Mutex<HashMap<magi_core::SessionId, SessionConversationProjection>>>,
    event_accepted_submissions: Arc<Mutex<Vec<AcceptedSubmissionRecord>>>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AcceptedSubmissionRecord {
    pub session: SessionAcceptanceRecord,
    pub task: Task,
    pub session_checkpointed: bool,
    pub task_checkpointed: bool,
    pub task_projection_generation_at_acceptance: u64,
}

impl PartialEq for AcceptedSubmissionRecord {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_vec(self).ok() == serde_json::to_vec(other).ok()
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AcceptedSubmissionJournal {
    schema_version: u32,
    records: Vec<AcceptedSubmissionRecord>,
}

impl Default for AcceptedSubmissionJournal {
    fn default() -> Self {
        Self {
            schema_version: ACCEPTED_SUBMISSION_JOURNAL_SCHEMA_VERSION,
            records: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionProjectionSnapshot {
    canonical_event_seq: u64,
    durable: SessionDurableState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sidecar: Option<SessionRuntimeSidecar>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionProjectionTransaction {
    schema_version: u32,
    transaction_id: String,
    writes: Vec<SessionProjectionWrite>,
    removals: Vec<SessionProjectionRemoval>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionProjectionWrite {
    path: PathBuf,
    content: String,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionProjectionRemovalKind {
    File,
    Directory,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionProjectionRemoval {
    path: PathBuf,
    kind: SessionProjectionRemovalKind,
}

#[derive(Clone, Debug, Default)]
struct SessionProjectionCache {
    snapshots: HashMap<magi_core::SessionId, (PathBuf, String)>,
    pending_removals: HashSet<PathBuf>,
    pending_event_removals: HashSet<PathBuf>,
    global: Option<(PathBuf, String)>,
    app_meta: Option<(PathBuf, String)>,
    workspace_meta: HashMap<String, (PathBuf, String)>,
}

const STATE_LAYOUT_VERSION: u32 = 2;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StateLayoutMarker {
    version: u32,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StateLayoutMigration {
    source_version: u32,
    target_version: u32,
}

/// 迁移阶段的完整输入快照。它在删除任何旧布局或未标记布局前先原子写入，
/// 因此进程可以在迁移任意一步退出后从同一份快照继续，不会再次猜测数据来源。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StateLayoutMigrationStaging {
    source_version: u32,
    target_version: u32,
    durable: SessionDurableState,
    sidecars: SessionExecutionSidecarStoreState,
    task_checkpoint: Option<serde_json::Value>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct UnmarkedSessionProjectionSnapshot {
    durable: SessionDurableState,
    #[serde(default)]
    sidecar: Option<SessionRuntimeSidecar>,
}

#[derive(Clone, Debug)]
struct UnmarkedSessionProjection {
    path: PathBuf,
    durable: SessionDurableState,
    sidecar: Option<SessionRuntimeSidecar>,
}

#[derive(Clone, Debug)]
struct LegacyStatePaths {
    global_sessions: PathBuf,
    session_sidecars: PathBuf,
    task_store: PathBuf,
    workspace_sessions: Vec<(String, PathBuf)>,
}

impl LegacyStatePaths {
    fn existing_paths(&self) -> Vec<PathBuf> {
        let mut paths = [
            self.global_sessions.clone(),
            self.session_sidecars.clone(),
            self.task_store.clone(),
        ]
        .into_iter()
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
        paths.extend(
            self.workspace_sessions
                .iter()
                .map(|(_, path)| path)
                .filter(|path| path.exists())
                .cloned(),
        );
        paths
    }
}

impl StateRepository {
    pub(crate) fn new(state_root: PathBuf) -> Self {
        Self {
            state_root,
            write_lock: Arc::new(Mutex::new(())),
            session_projection_cache: Arc::new(Mutex::new(SessionProjectionCache::default())),
            session_event_cache: Arc::new(Mutex::new(HashMap::new())),
            event_accepted_submissions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn save_session_projection_state(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
    ) -> Result<(), DaemonError> {
        let workspace_roots = self.workspace_projection_roots()?;
        self.save_session_projection_parts(durable, sidecars, &workspace_roots, true)
    }

    /// v1 -> v2 converter 的唯一事件初始化入口。正常 v2 checkpoint 禁止调用。
    fn initialize_session_events(&self, durable: &SessionDurableState) -> Result<(), DaemonError> {
        let mut turns_by_session = HashMap::<SessionId, Vec<_>>::new();
        for turn in &durable.canonical_turns {
            turns_by_session
                .entry(turn.session_id.clone())
                .or_default()
                .push(turn.clone());
        }
        for session in &durable.sessions {
            let mut turns = turns_by_session
                .remove(&session.session_id)
                .unwrap_or_default();
            turns.sort_by(|left, right| {
                left.turn_seq
                    .cmp(&right.turn_seq)
                    .then_with(|| left.turn_id.cmp(&right.turn_id))
            });
            if turns.is_empty() {
                continue;
            }
            let mut mutations = Vec::with_capacity(turns.len());
            for next in turns {
                if next.status == magi_session_store::CanonicalTurnStatus::Superseded {
                    // 旧布局只保存最终快照，没有记录 Superseded 必须经过 Cancelled
                    // 的中间事件；迁移事件日志时补齐这段真实状态机路径。
                    let mut cancelled = next.clone();
                    cancelled.status = magi_session_store::CanonicalTurnStatus::Cancelled;
                    mutations.push(CanonicalTurnMutation {
                        previous: None,
                        next: cancelled.clone(),
                    });
                    mutations.push(CanonicalTurnMutation {
                        previous: Some(cancelled),
                        next,
                    });
                } else {
                    mutations.push(CanonicalTurnMutation {
                        previous: None,
                        next,
                    });
                }
            }
            self.append_canonical_turn_transaction(&session.session_id, &mutations)
                .map_err(|error| {
                    DaemonError::internal(format!(
                        "初始化迁移 canonical 事件失败 {}: {error}",
                        session.session_id
                    ))
                })?;
        }
        if !turns_by_session.is_empty() {
            return Err(DaemonError::internal(
                "迁移 canonical turn 引用了不存在的 session".to_string(),
            ));
        }
        Ok(())
    }

    pub(crate) fn load_session_projections(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<(SessionDurableState, SessionExecutionSidecarStoreState), DaemonError> {
        self.recover_session_projection_transaction(workspace_roots)?;
        let mut durable = SessionDurableState::default();
        let mut sidecars = SessionExecutionSidecarStoreState::default();
        let mut cache = SessionProjectionCache::default();
        let mut event_cache = HashMap::new();
        let mut event_accepted_submissions = Vec::new();
        let mut loaded_snapshots = HashMap::<magi_core::SessionId, serde_json::Value>::new();
        let workspace_root_by_id = workspace_roots.iter().cloned().collect::<HashMap<_, _>>();
        let mut roots = vec![(String::new(), self.state_root.clone())];
        roots.extend(workspace_roots.iter().cloned());

        for (workspace_id, workspace_root) in &roots {
            let projection_root = if workspace_id.is_empty() {
                self.session_projection_root()
            } else {
                workspace_root.join(".magi").join("session-projections")
            };
            if projection_root.exists() {
                for entry in fs::read_dir(&projection_root)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.extension().and_then(|value| value.to_str()) != Some("json") {
                        continue;
                    }
                    let content = fs::read_to_string(&path)?;
                    let mut snapshot: SessionProjectionSnapshot = serde_json::from_str(&content)
                        .map_err(|error| {
                            DaemonError::internal(format!(
                                "解析 session projection 失败 {}: {error}",
                                path.display()
                            ))
                        })?;
                    let session_id = Self::validate_session_projection(&snapshot, &path)?;
                    let value =
                        serde_json::from_str::<serde_json::Value>(&content).map_err(|error| {
                            DaemonError::internal(format!(
                                "解析 session projection 失败 {}: {error}",
                                path.display()
                            ))
                        })?;
                    if let Some(previous_value) = loaded_snapshots.get(&session_id) {
                        if previous_value != &value {
                            let previous_path = cache
                                .snapshots
                                .get(&session_id)
                                .map(|(path, _)| path.display().to_string())
                                .unwrap_or_else(|| "<unknown>".to_string());
                            return Err(DaemonError::internal(format!(
                                "session {} 存在冲突的重复 projection: {} 与 {}",
                                session_id,
                                previous_path,
                                path.display()
                            )));
                        }
                        let session = snapshot
                            .durable
                            .sessions
                            .first()
                            .expect("projection validation requires one session");
                        let expected_path = self.session_projection_path(
                            &session_id,
                            session.workspace_id.as_deref(),
                            &workspace_root_by_id,
                        );
                        let replace_cached = path == expected_path
                            && cache
                                .snapshots
                                .get(&session_id)
                                .is_some_and(|(cached_path, _)| cached_path != &expected_path);
                        if replace_cached {
                            if let Some((old_path, _)) =
                                cache.snapshots.insert(session_id.clone(), (path, content))
                            {
                                cache.pending_removals.insert(old_path);
                            }
                        } else {
                            cache.pending_removals.insert(path);
                        }
                        continue;
                    }
                    let event_root = self.session_event_root(&session_id);
                    let event_projection =
                        SessionConversationProjection::load(&event_root, &session_id)?;
                    event_accepted_submissions.extend(
                        event_projection.accepted_submissions().iter().cloned().map(
                            |mut record| {
                                // event segment 已经恢复了 session/sidecar，只有 task
                                // checkpoint 仍需要在启动后按 TaskStore 代际收敛。
                                record.session_checkpointed = true;
                                record
                            },
                        ),
                    );
                    if snapshot.canonical_event_seq > event_projection.last_event_seq() {
                        return Err(DaemonError::internal(format!(
                            "session projection 的 canonical event 游标超前: {} > {} ({})",
                            snapshot.canonical_event_seq,
                            event_projection.last_event_seq(),
                            path.display()
                        )));
                    }
                    Self::validate_cached_canonical_projection(
                        &snapshot.durable.canonical_turns,
                        snapshot.canonical_event_seq,
                        &event_projection,
                        &path,
                    )?;
                    snapshot.durable.canonical_turns = event_projection.canonical_turns().to_vec();
                    if snapshot.canonical_event_seq < event_projection.last_event_seq()
                        && let Some(sidecar) = snapshot.sidecar.as_mut()
                    {
                        SessionStore::advance_sidecar_projection_from_canonical(
                            sidecar,
                            event_projection.canonical_turns(),
                        )
                        .map_err(|error| {
                            DaemonError::internal(format!(
                                "canonical event 无法推进 sidecar projection {}: {error}",
                                path.display()
                            ))
                        })?;
                    }
                    if let Some(sidecar) = snapshot.sidecar {
                        sidecars.upsert_runtime_sidecar(sidecar);
                    }
                    durable.append_state_without_current(snapshot.durable.clone());
                    for record in event_projection.accepted_submissions() {
                        Self::merge_event_acceptance_into_projection(
                            &mut durable,
                            &mut sidecars,
                            record,
                        );
                    }
                    loaded_snapshots.insert(session_id.clone(), value);
                    event_cache.insert(session_id.clone(), event_projection);
                    cache.snapshots.insert(session_id, (path, content));
                }
            }

            if !workspace_id.is_empty() {
                let meta_path = workspace_root
                    .join(".magi")
                    .join("session-workspace-meta.json");
                if meta_path.exists() {
                    let content = fs::read_to_string(&meta_path)?;
                    let meta: SessionDurableState = serde_json::from_str(&content)?;
                    durable.notifications.extend(meta.notifications);
                    cache
                        .workspace_meta
                        .insert(workspace_id.clone(), (meta_path, content));
                }
            }
        }

        // accepted event 可能已经 durable，但对应的 session projection 尚未写入。
        // 事件目录本身是权威源，必须从目录发现并恢复这类 session，不能把它误判成
        // 没有归属的孤立目录后丢弃。
        let event_parent = self.state_root.join("session-events");
        if event_parent.exists() {
            let mut event_roots = fs::read_dir(&event_parent)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()?;
            event_roots.sort();
            for event_root in event_roots {
                if !event_root.is_dir() {
                    return Err(DaemonError::internal(format!(
                        "canonical event 根目录包含非 session 目录: {}",
                        event_root.display()
                    )));
                }
                let event_projection = SessionConversationProjection::load_from_root(&event_root)?;
                let session_id = event_projection.session_id().cloned().ok_or_else(|| {
                    DaemonError::internal(format!(
                        "canonical event projection 缺少 session 归属: {}",
                        event_root.display()
                    ))
                })?;
                if self.session_event_root(&session_id) != event_root {
                    return Err(DaemonError::internal(format!(
                        "canonical event 目录与 session 归属不一致: {}",
                        event_root.display()
                    )));
                }
                if event_cache.contains_key(&session_id) {
                    continue;
                }
                if event_projection.accepted_submissions().is_empty() {
                    // 没有 accepted 事实时无法从事件本身重建 session 元数据；最终由
                    // coverage 校验报告该目录确实是损坏的孤立事件日志。
                    continue;
                }
                for record in event_projection.accepted_submissions() {
                    let mut record = record.clone();
                    record.session_checkpointed = true;
                    Self::merge_event_acceptance_into_projection(
                        &mut durable,
                        &mut sidecars,
                        &record,
                    );
                    event_accepted_submissions.push(record);
                }
                for turn in event_projection.canonical_turns() {
                    if let Some(existing) = durable
                        .canonical_turns
                        .iter()
                        .find(|existing| existing.turn_id == turn.turn_id)
                    {
                        if existing != turn {
                            return Err(DaemonError::internal(format!(
                                "canonical event 与 session projection 的 turn 冲突: {}",
                                turn.turn_id
                            )));
                        }
                    } else {
                        durable.canonical_turns.push(turn.clone());
                    }
                }
                event_cache.insert(session_id, event_projection);
            }
        }

        let current_path = self.state_root.join("session-current.json");
        if current_path.exists() {
            let content = fs::read_to_string(&current_path)?;
            let current: Option<magi_core::SessionId> =
                serde_json::from_str(&content).map_err(|error| {
                    DaemonError::internal(format!(
                        "解析 session current state 失败 {}: {error}",
                        current_path.display()
                    ))
                })?;
            durable.current_session_id = current;
            cache.global = Some((current_path, content));
        }

        let app_meta_path = self.state_root.join("session-app-meta.json");
        if app_meta_path.exists() {
            let content = fs::read_to_string(&app_meta_path)?;
            let meta: SessionDurableState = serde_json::from_str(&content)?;
            durable.notifications.extend(meta.notifications);
            cache.app_meta = Some((app_meta_path, content));
        }

        if let Some(current_session_id) = durable.current_session_id.as_ref()
            && !durable
                .sessions
                .iter()
                .any(|session| &session.session_id == current_session_id)
        {
            return Err(DaemonError::internal(format!(
                "session current 指向不存在的 session: {current_session_id}"
            )));
        }

        *self
            .session_projection_cache
            .lock()
            .expect("session projection cache lock poisoned") = cache;
        *self
            .session_event_cache
            .lock()
            .expect("session event cache lock poisoned") = event_cache;
        *self
            .event_accepted_submissions
            .lock()
            .expect("event accepted submission cache lock poisoned") = event_accepted_submissions;
        Ok((durable, sidecars))
    }

    fn merge_event_acceptance_into_projection(
        durable: &mut SessionDurableState,
        sidecars: &mut SessionExecutionSidecarStoreState,
        record: &AcceptedSubmissionRecord,
    ) {
        match durable
            .sessions
            .iter_mut()
            .find(|session| session.session_id == record.session.session.session_id)
        {
            Some(existing) if record.session.session.updated_at.0 > existing.updated_at.0 => {
                *existing = record.session.session.clone();
            }
            Some(_) => {}
            None => durable.sessions.push(record.session.session.clone()),
        }
        if !durable
            .timeline
            .iter()
            .any(|entry| entry.entry_id == record.session.timeline_entry.entry_id)
        {
            durable.timeline.push(record.session.timeline_entry.clone());
        }
        let should_update_sidecar = sidecars
            .runtime_sidecar(&record.session.sidecar.session_id)
            .is_none_or(|existing| record.session.sidecar.updated_at.0 > existing.updated_at.0);
        if should_update_sidecar {
            sidecars.upsert_runtime_sidecar(record.session.sidecar.clone());
        }
    }

    /// 校验事件目录都有已恢复的 session 归属。首次 accepted 持久化可能在事件写入后、
    /// projection 写入前崩溃；此时 accepted journal 会先把 session 恢复进内存，再调用
    /// 本方法确认该孤立目录确实有 WAL 归属。其余未知目录一律按状态损坏处理。
    pub(crate) fn validate_session_event_log_coverage(
        &self,
        durable: &SessionDurableState,
    ) -> Result<(), DaemonError> {
        let event_parent = self.state_root.join("session-events");
        if !event_parent.exists() {
            return Ok(());
        }
        let expected = durable
            .sessions
            .iter()
            .map(|session| {
                (
                    self.session_event_root(&session.session_id),
                    session.session_id.clone(),
                )
            })
            .collect::<HashMap<_, _>>();
        for entry in fs::read_dir(&event_parent)? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_dir() {
                return Err(DaemonError::internal(format!(
                    "canonical event 根目录包含非 session 目录: {}",
                    path.display()
                )));
            }
            let session_id = expected.get(&path).ok_or_else(|| {
                DaemonError::internal(format!(
                    "canonical event 目录没有 session 或 accepted WAL 归属: {}",
                    path.display()
                ))
            })?;
            SessionConversationProjection::load(&path, session_id)?;
        }
        Ok(())
    }

    pub(crate) fn workspace_projection_roots(
        &self,
    ) -> Result<HashMap<String, PathBuf>, DaemonError> {
        let state = self.load_workspace_durable_state()?;
        Ok(state
            .workspaces
            .into_iter()
            .map(|workspace| {
                (
                    workspace.workspace_id.to_string(),
                    workspace.native_root_path(),
                )
            })
            .collect())
    }

    pub(crate) fn migrate_legacy_state_layout(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<(), DaemonError> {
        self.recover_session_projection_transaction(workspace_roots)?;
        let layout_path = self.state_root.join("state-layout.json");
        let migration_path = self.state_root.join("state-layout-migration.json");
        let staging_path = self.state_layout_migration_staging_path();
        let legacy = self.legacy_state_paths(workspace_roots);
        let legacy_paths = legacy.existing_paths();

        if layout_path.exists() {
            let marker: StateLayoutMarker = self.read_json_strict(&layout_path)?;
            if marker.version != STATE_LAYOUT_VERSION {
                return Err(DaemonError::internal(format!(
                    "不支持的 state layout 版本: {}",
                    marker.version
                )));
            }
            if migration_path.exists() {
                self.remove_legacy_sources(&legacy)?;
                self.remove_file_if_exists(&migration_path)?;
                self.remove_file_if_exists(&staging_path)?;
                return Ok(());
            }
            if !legacy_paths.is_empty() {
                return Err(DaemonError::internal(format!(
                    "state layout v2 已提交，但仍发现旧布局文件: {}",
                    legacy_paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            return Ok(());
        }

        let resuming = migration_path.exists();
        if resuming {
            let migration: StateLayoutMigration = self.read_json_strict(&migration_path)?;
            if migration.source_version != 1 || migration.target_version != STATE_LAYOUT_VERSION {
                return Err(DaemonError::internal(
                    "state layout migration 标记版本不合法".to_string(),
                ));
            }
        }

        let has_unmarked_new_layout = if resuming && !staging_path.exists() {
            // 旧版本没有 staging 快照。其迁移标记只表示“从旧布局重做”，
            // 因而目录中的半成品不能再被当作事实来源。
            self.clear_uncommitted_new_layout(workspace_roots)?;
            false
        } else {
            self.new_layout_exists(workspace_roots)
        };
        if legacy_paths.is_empty() && !has_unmarked_new_layout && !resuming {
            self.write_json_atomically(
                layout_path,
                &StateLayoutMarker {
                    version: STATE_LAYOUT_VERSION,
                },
            )?;
            return Ok(());
        }

        let staging = if resuming && staging_path.exists() {
            let mut staging: StateLayoutMigrationStaging = self.read_json_strict(&staging_path)?;
            if staging.source_version != 1 || staging.target_version != STATE_LAYOUT_VERSION {
                return Err(DaemonError::internal(
                    "state layout migration staging 版本不合法".to_string(),
                ));
            }
            // 迁移 staging 可能由旧实现生成：旧实现从 HashMap 合并 task/lease
            // 后没有保持 checkpoint 的 canonical ID 顺序。恢复 projection 会
            // 按 canonical 顺序返回，二者若直接比较会把纯顺序差异误判为数据损坏。
            // 在继续迁移前统一 staging 表示，确保中断恢复与首次迁移走同一合同。
            if let Some(task_checkpoint) = staging.task_checkpoint.as_mut() {
                *task_checkpoint =
                    TaskStore::restore_legacy_checkpoint(task_checkpoint)?.checkpoint();
            }
            staging
        } else {
            let (durable, sidecars) = if legacy_paths.is_empty() {
                self.build_unmarked_session_state(workspace_roots, has_unmarked_new_layout)?
            } else {
                let mut durable = self.load_legacy_session_state(&legacy)?;
                let mut sidecars = if legacy.session_sidecars.exists() {
                    self.read_json_strict(&legacy.session_sidecars)?
                } else {
                    SessionExecutionSidecarStoreState::default()
                };
                let projections = if has_unmarked_new_layout {
                    self.read_unmarked_session_projections(workspace_roots)?
                } else {
                    Vec::new()
                };
                Self::retain_reachable_legacy_sidecars(&mut sidecars, &durable, &projections);
                if has_unmarked_new_layout {
                    self.merge_unmarked_session_projections(
                        &mut durable,
                        &mut sidecars,
                        &projections,
                    )?;
                }
                let legacy_store = SessionStore::convert_v1_persisted_parts(durable, sidecars)
                    .map_err(|error| {
                        DaemonError::internal(format!("迁移旧 session 状态失败: {error}"))
                    })?;
                let mut durable = legacy_store.durable_state();
                let sidecars = legacy_store.execution_sidecar_store_state();
                if has_unmarked_new_layout {
                    let (current, notifications) =
                        self.read_unmarked_session_metadata(workspace_roots)?;
                    self.merge_unmarked_notifications(&mut durable, notifications)?;
                    self.merge_current_session_id(&mut durable, current)?;
                }
                (durable, sidecars)
            };
            let task_checkpoint = self.merge_task_checkpoint(&legacy, has_unmarked_new_layout)?;
            let staging = StateLayoutMigrationStaging {
                source_version: 1,
                target_version: STATE_LAYOUT_VERSION,
                durable,
                sidecars,
                task_checkpoint,
            };
            self.write_json_atomically(staging_path.clone(), &staging)?;
            staging
        };

        if !resuming {
            self.archive_legacy_sources(&legacy)?;
            self.write_json_atomically(
                migration_path.clone(),
                &StateLayoutMigration {
                    source_version: 1,
                    target_version: STATE_LAYOUT_VERSION,
                },
            )?;
        }

        self.clear_uncommitted_new_layout(workspace_roots)?;
        self.initialize_session_events(&staging.durable)?;
        let workspace_root_map = workspace_roots.iter().cloned().collect::<HashMap<_, _>>();
        self.save_session_projection_parts(
            &staging.durable,
            &staging.sidecars,
            &workspace_root_map,
            false,
        )?;

        if let Some(value) = staging.task_checkpoint.as_ref() {
            let task_store = TaskStore::restore_legacy_checkpoint(value)?;
            let snapshot = task_store.snapshot();
            self.checkpoint_task_store_snapshot_inner(&snapshot, false)?;
        }

        self.validate_migrated_layout(
            workspace_roots,
            &staging.durable,
            staging.task_checkpoint.as_ref(),
        )?;
        self.write_json_atomically(
            layout_path,
            &StateLayoutMarker {
                version: STATE_LAYOUT_VERSION,
            },
        )?;
        self.remove_legacy_sources(&legacy)?;
        self.remove_file_if_exists(&migration_path)?;
        self.remove_file_if_exists(&staging_path)?;
        Ok(())
    }

    fn state_layout_migration_staging_path(&self) -> PathBuf {
        self.state_root.join("state-layout-migration-staging.json")
    }

    fn build_unmarked_session_state(
        &self,
        workspace_roots: &[(String, PathBuf)],
        has_unmarked_new_layout: bool,
    ) -> Result<(SessionDurableState, SessionExecutionSidecarStoreState), DaemonError> {
        if !has_unmarked_new_layout {
            return Ok((
                SessionDurableState::default(),
                SessionExecutionSidecarStoreState::default(),
            ));
        }
        let projections = self.read_unmarked_session_projections(workspace_roots)?;
        let mut durable = SessionDurableState::default();
        let mut sidecars = SessionExecutionSidecarStoreState::default();
        self.merge_unmarked_session_projections(&mut durable, &mut sidecars, &projections)?;
        let (current, notifications) = self.read_unmarked_session_metadata(workspace_roots)?;
        self.merge_unmarked_notifications(&mut durable, notifications)?;
        self.merge_current_session_id(&mut durable, current)?;
        Ok((durable, sidecars))
    }

    fn retain_reachable_legacy_sidecars(
        sidecars: &mut SessionExecutionSidecarStoreState,
        durable: &SessionDurableState,
        projections: &[UnmarkedSessionProjection],
    ) {
        let mut reachable = durable
            .sessions
            .iter()
            .map(|session| session.session_id.clone())
            .collect::<HashSet<_>>();
        reachable.extend(
            projections
                .iter()
                .filter_map(|projection| projection.durable.sessions.first())
                .map(|session| session.session_id.clone()),
        );
        sidecars
            .runtime_sidecars
            .retain(|sidecar| reachable.contains(&sidecar.session_id));
    }

    fn read_unmarked_session_projections(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<Vec<UnmarkedSessionProjection>, DaemonError> {
        let mut roots = vec![self.session_projection_root()];
        roots.extend(
            workspace_roots
                .iter()
                .map(|(_, root)| root.join(".magi").join("session-projections")),
        );
        let mut by_session = HashMap::<SessionId, UnmarkedSessionProjection>::new();
        for root in roots {
            if !root.exists() {
                continue;
            }
            for entry in fs::read_dir(&root)? {
                let entry = entry?;
                let path = entry.path();
                if !entry.file_type()?.is_file()
                    || path.extension().and_then(|extension| extension.to_str()) != Some("json")
                {
                    continue;
                }
                let content = fs::read_to_string(&path)?;
                let parsed: UnmarkedSessionProjectionSnapshot = serde_json::from_str(&content)
                    .map_err(|error| {
                        DaemonError::internal(format!(
                            "解析未标记 session projection 失败 {}: {error}",
                            path.display()
                        ))
                    })?;
                let snapshot = SessionProjectionSnapshot {
                    canonical_event_seq: 0,
                    durable: parsed.durable.clone(),
                    sidecar: parsed.sidecar.clone(),
                };
                let session_id = Self::validate_session_projection(&snapshot, &path)?;
                let candidate = UnmarkedSessionProjection {
                    path: path.clone(),
                    durable: parsed.durable,
                    sidecar: parsed.sidecar,
                };
                if let Some(previous) = by_session.get(&session_id) {
                    let same_durable = serde_json::to_value(&previous.durable)
                        .map_err(DaemonError::from)?
                        == serde_json::to_value(&candidate.durable).map_err(DaemonError::from)?;
                    let same_sidecar = serde_json::to_value(&previous.sidecar)
                        .map_err(DaemonError::from)?
                        == serde_json::to_value(&candidate.sidecar).map_err(DaemonError::from)?;
                    if !same_durable || !same_sidecar {
                        return Err(DaemonError::internal(format!(
                            "未标记 session projection 包含冲突 session {}: {} 与 {}",
                            session_id,
                            previous.path.display(),
                            path.display()
                        )));
                    }
                    continue;
                }
                by_session.insert(session_id, candidate);
            }
        }
        Ok(by_session.into_values().collect())
    }

    fn read_unmarked_session_metadata(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<(Option<SessionId>, Vec<serde_json::Value>), DaemonError> {
        let current_path = self.state_root.join("session-current.json");
        let current = if current_path.exists() {
            Some(self.read_json_strict(&current_path)?)
        } else {
            None
        };
        let mut notifications = Vec::new();
        let mut metadata_paths = vec![self.state_root.join("session-app-meta.json")];
        metadata_paths.extend(
            workspace_roots
                .iter()
                .map(|(_, root)| root.join(".magi").join("session-workspace-meta.json")),
        );
        for path in metadata_paths {
            if !path.exists() {
                continue;
            }
            let meta: SessionDurableState = self.read_json_strict(&path)?;
            notifications.extend(
                meta.notifications
                    .into_iter()
                    .map(|notification| serde_json::to_value(notification))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DaemonError::from)?,
            );
        }
        Ok((current.flatten(), notifications))
    }

    fn merge_unmarked_session_projections(
        &self,
        durable: &mut SessionDurableState,
        sidecars: &mut SessionExecutionSidecarStoreState,
        projections: &[UnmarkedSessionProjection],
    ) -> Result<(), DaemonError> {
        for projection in projections {
            let session_id = projection
                .durable
                .sessions
                .first()
                .expect("validated session projection must contain one session")
                .session_id
                .clone();
            let existing = durable.durable_state_for_session(&session_id);
            let selected = if existing.sessions.is_empty() {
                projection.durable.clone()
            } else {
                let same = serde_json::to_value(&existing).map_err(DaemonError::from)?
                    == serde_json::to_value(&projection.durable).map_err(DaemonError::from)?;
                if same {
                    existing
                } else {
                    let existing_updated = existing.sessions[0].updated_at.0;
                    let candidate_updated = projection.durable.sessions[0].updated_at.0;
                    match candidate_updated.cmp(&existing_updated) {
                        std::cmp::Ordering::Greater => projection.durable.clone(),
                        std::cmp::Ordering::Less => existing,
                        std::cmp::Ordering::Equal => {
                            return Err(DaemonError::internal(format!(
                                "session {} 新旧布局更新时间相同但事实冲突",
                                session_id
                            )));
                        }
                    }
                }
            };
            Self::replace_session_facts(durable, &session_id, selected);

            if let Some(candidate) = projection.sidecar.clone() {
                match sidecars.runtime_sidecar(&session_id) {
                    None => sidecars.upsert_runtime_sidecar(candidate),
                    Some(existing) => {
                        let same = serde_json::to_value(&existing).map_err(DaemonError::from)?
                            == serde_json::to_value(&candidate).map_err(DaemonError::from)?;
                        if same {
                            continue;
                        }
                        if candidate.updated_at.0 > existing.updated_at.0 {
                            sidecars.upsert_runtime_sidecar(candidate);
                        } else if candidate.updated_at.0 == existing.updated_at.0 {
                            return Err(DaemonError::internal(format!(
                                "session {} 新旧 sidecar 更新时间相同但事实冲突",
                                session_id
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn replace_session_facts(
        durable: &mut SessionDurableState,
        session_id: &SessionId,
        selected: SessionDurableState,
    ) {
        durable
            .sessions
            .retain(|session| &session.session_id != session_id);
        durable
            .timeline
            .retain(|entry| &entry.session_id != session_id);
        durable
            .canonical_turns
            .retain(|turn| &turn.session_id != session_id);
        durable
            .notifications
            .retain(|notification| notification.session_id.as_ref() != Some(session_id));
        durable.goals.retain(|goal| &goal.session_id != session_id);
        durable.plans.retain(|plan| &plan.session_id != session_id);
        let removed_thread_ids = durable
            .thread_registry
            .iter()
            .filter(|thread| &thread.session_id == session_id)
            .map(|thread| thread.thread_id.clone())
            .collect::<HashSet<_>>();
        durable
            .thread_registry
            .retain(|thread| &thread.session_id != session_id);
        let selected_thread_ids = selected
            .thread_registry
            .iter()
            .map(|thread| thread.thread_id.clone())
            .collect::<HashSet<_>>();
        durable.thread_context_checkpoints.retain(|checkpoint| {
            !removed_thread_ids.contains(&checkpoint.thread_id)
                && !selected_thread_ids.contains(&checkpoint.thread_id)
        });
        durable.append_state_without_current(selected);
    }

    fn merge_unmarked_notifications(
        &self,
        durable: &mut SessionDurableState,
        incoming: Vec<serde_json::Value>,
    ) -> Result<(), DaemonError> {
        for value in incoming {
            let notification: magi_session_store::NotificationRecord =
                serde_json::from_value(value).map_err(DaemonError::from)?;
            if let Some(existing) = durable
                .notifications
                .iter_mut()
                .find(|existing| existing.notification_id == notification.notification_id)
            {
                let existing_value = serde_json::to_value(&*existing).map_err(DaemonError::from)?;
                let incoming_value =
                    serde_json::to_value(&notification).map_err(DaemonError::from)?;
                if existing_value == incoming_value {
                    continue;
                }
                if notification.created_at.0 > existing.created_at.0 {
                    *existing = notification;
                } else if notification.created_at.0 == existing.created_at.0 {
                    return Err(DaemonError::internal(format!(
                        "notification {} 新旧布局创建时间相同但事实冲突",
                        notification.notification_id
                    )));
                }
            } else {
                durable.notifications.push(notification);
            }
        }
        Ok(())
    }

    fn merge_current_session_id(
        &self,
        durable: &mut SessionDurableState,
        incoming: Option<SessionId>,
    ) -> Result<(), DaemonError> {
        let Some(incoming) = incoming else {
            return Ok(());
        };
        if !durable
            .sessions
            .iter()
            .any(|session| session.session_id == incoming)
        {
            return Err(DaemonError::internal(format!(
                "未标记 session current 指向不存在的 session: {incoming}"
            )));
        }
        let Some(existing) = durable.current_session_id.clone() else {
            durable.current_session_id = Some(incoming);
            return Ok(());
        };
        if existing == incoming {
            return Ok(());
        }
        let updated_at = |session_id: &SessionId| {
            durable
                .sessions
                .iter()
                .find(|session| &session.session_id == session_id)
                .map(|session| session.updated_at.0)
                .unwrap_or(0)
        };
        match updated_at(&incoming).cmp(&updated_at(&existing)) {
            std::cmp::Ordering::Greater => durable.current_session_id = Some(incoming),
            std::cmp::Ordering::Equal => {
                return Err(DaemonError::internal(format!(
                    "新旧布局 current session 冲突: {existing} 与 {incoming}"
                )));
            }
            std::cmp::Ordering::Less => {}
        }
        Ok(())
    }

    fn merge_task_checkpoint(
        &self,
        legacy: &LegacyStatePaths,
        has_unmarked_new_layout: bool,
    ) -> Result<Option<serde_json::Value>, DaemonError> {
        let legacy_snapshot = if legacy.task_store.exists() {
            Some(
                TaskStore::restore_legacy_checkpoint(&self.read_json_strict(&legacy.task_store)?)?
                    .snapshot(),
            )
        } else {
            None
        };
        let new_snapshot = if has_unmarked_new_layout {
            TaskStore::restore_unmarked_projection_directory_for_migration(
                &self.task_store_projection_path(),
            )?
            .map(|store| store.snapshot())
        } else {
            None
        };
        let Some(snapshot) = (match (legacy_snapshot, new_snapshot) {
            (None, None) => None,
            (Some(snapshot), None) | (None, Some(snapshot)) => Some(snapshot),
            (Some(legacy), Some(candidate)) => Some(self.merge_task_snapshots(legacy, candidate)?),
        }) else {
            return Ok(None);
        };
        let value = serde_json::json!({
            "tasks": snapshot.tasks,
            "leases": snapshot.leases,
        });
        TaskStore::restore_legacy_checkpoint(&value)?;
        Ok(Some(value))
    }

    fn merge_task_snapshots(
        &self,
        legacy: TaskStoreSnapshot,
        candidate: TaskStoreSnapshot,
    ) -> Result<TaskStoreSnapshot, DaemonError> {
        let mut tasks = legacy
            .tasks
            .into_iter()
            .map(|task| (task.task_id.clone(), task))
            .collect::<HashMap<_, _>>();
        for task in candidate.tasks {
            match tasks.get(&task.task_id) {
                None => {
                    tasks.insert(task.task_id.clone(), task);
                }
                Some(existing) => {
                    let same = serde_json::to_value(existing).map_err(DaemonError::from)?
                        == serde_json::to_value(&task).map_err(DaemonError::from)?;
                    if same {
                        continue;
                    }
                    if task.updated_at.0 > existing.updated_at.0 {
                        tasks.insert(task.task_id.clone(), task);
                    } else if task.updated_at.0 == existing.updated_at.0 {
                        return Err(DaemonError::internal(format!(
                            "新旧 task 更新时间相同但事实冲突: {}",
                            task.task_id
                        )));
                    }
                }
            }
        }
        let mut leases = legacy
            .leases
            .into_iter()
            .map(|lease| (lease.lease_id.clone(), lease))
            .collect::<HashMap<_, _>>();
        for lease in candidate.leases {
            match leases.get(&lease.lease_id) {
                None => {
                    leases.insert(lease.lease_id.clone(), lease);
                }
                Some(existing) => {
                    let same = serde_json::to_value(existing).map_err(DaemonError::from)?
                        == serde_json::to_value(&lease).map_err(DaemonError::from)?;
                    if same {
                        continue;
                    }
                    if lease.heartbeat_at.0 > existing.heartbeat_at.0 {
                        leases.insert(lease.lease_id.clone(), lease);
                    } else if lease.heartbeat_at.0 == existing.heartbeat_at.0 {
                        return Err(DaemonError::internal(format!(
                            "新旧 lease heartbeat 相同但事实冲突: {}",
                            lease.lease_id
                        )));
                    }
                }
            }
        }
        Ok(TaskStoreSnapshot {
            tasks: {
                let mut tasks = tasks.into_values().collect::<Vec<_>>();
                tasks.sort_by(|left, right| left.task_id.as_str().cmp(right.task_id.as_str()));
                tasks
            },
            leases: {
                let mut leases = leases.into_values().collect::<Vec<_>>();
                leases.sort_by(|left, right| left.lease_id.as_str().cmp(right.lease_id.as_str()));
                leases
            },
        })
    }

    fn session_projection_transaction_path(&self) -> PathBuf {
        self.state_root.join("session-projection-transaction.json")
    }

    fn recover_session_projection_transaction(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<(), DaemonError> {
        let path = self.session_projection_transaction_path();
        if !path.exists() {
            return Ok(());
        }
        let roots = workspace_roots.iter().cloned().collect::<HashMap<_, _>>();
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let transaction: SessionProjectionTransaction = serde_json::from_slice(&fs::read(&path)?)
            .map_err(|error| {
            DaemonError::internal(format!(
                "session projection transaction 损坏，拒绝继续启动 {}: {error}",
                path.display()
            ))
        })?;
        self.apply_session_projection_transaction_locked(&transaction, &roots)
    }

    fn commit_session_projection_transaction_locked(
        &self,
        transaction: &SessionProjectionTransaction,
        workspace_roots: &HashMap<String, PathBuf>,
    ) -> Result<(), DaemonError> {
        if transaction.writes.is_empty() && transaction.removals.is_empty() {
            return Ok(());
        }
        self.validate_session_projection_transaction(transaction, workspace_roots)?;
        let path = self.session_projection_transaction_path();
        fs::create_dir_all(&self.state_root)?;
        magi_core::fs_atomic::write_atomic(
            &path,
            serde_json::to_vec_pretty(transaction).map_err(DaemonError::from)?,
        )?;
        self.apply_session_projection_transaction_locked(transaction, workspace_roots)
    }

    fn apply_session_projection_transaction_locked(
        &self,
        transaction: &SessionProjectionTransaction,
        workspace_roots: &HashMap<String, PathBuf>,
    ) -> Result<(), DaemonError> {
        self.validate_session_projection_transaction(transaction, workspace_roots)?;
        for write in &transaction.writes {
            if let Some(parent) = write.path.parent() {
                fs::create_dir_all(parent)?;
            }
            magi_core::fs_atomic::write_atomic(&write.path, write.content.as_bytes())?;
        }
        for removal in &transaction.removals {
            match removal.kind {
                SessionProjectionRemovalKind::File => {
                    Self::remove_file_durable(&removal.path)?;
                }
                SessionProjectionRemovalKind::Directory => {
                    Self::remove_directory_durable(&removal.path)?;
                }
            }
        }
        Self::remove_file_durable(&self.session_projection_transaction_path())
    }

    fn validate_session_projection_transaction(
        &self,
        transaction: &SessionProjectionTransaction,
        workspace_roots: &HashMap<String, PathBuf>,
    ) -> Result<(), DaemonError> {
        if transaction.schema_version != SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION
            || transaction.transaction_id.trim().is_empty()
        {
            return Err(DaemonError::internal(
                "session projection transaction schema 或 transactionId 无效".to_string(),
            ));
        }
        let workspace_state_roots = workspace_roots
            .values()
            .map(|root| root.join(".magi"))
            .collect::<Vec<_>>();
        let allowed = |path: &Path| {
            path.starts_with(&self.state_root)
                || workspace_state_roots
                    .iter()
                    .any(|root| path.starts_with(root))
        };
        let mut write_paths = HashSet::new();
        for write in &transaction.writes {
            if !allowed(&write.path) || !write_paths.insert(write.path.clone()) {
                return Err(DaemonError::internal(format!(
                    "session projection transaction 包含越界或重复写路径: {}",
                    write.path.display()
                )));
            }
        }
        let mut removal_paths = HashSet::new();
        for removal in &transaction.removals {
            if !allowed(&removal.path)
                || write_paths.contains(&removal.path)
                || !removal_paths.insert(removal.path.clone())
                || removal.path == self.state_root
                || workspace_state_roots
                    .iter()
                    .any(|root| &removal.path == root)
            {
                return Err(DaemonError::internal(format!(
                    "session projection transaction 包含越界、冲突或重复删除路径: {}",
                    removal.path.display()
                )));
            }
        }
        Ok(())
    }

    fn sync_parent_directory(path: &Path) {
        if let Some(parent) = path.parent()
            && let Ok(directory) = fs::File::open(parent)
        {
            let _ = directory.sync_all();
        }
    }

    fn remove_file_durable(path: &Path) -> Result<(), DaemonError> {
        match fs::remove_file(path) {
            Ok(()) => Self::sync_parent_directory(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn remove_directory_durable(path: &Path) -> Result<(), DaemonError> {
        match fs::remove_dir_all(path) {
            Ok(()) => Self::sync_parent_directory(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn legacy_state_paths(&self, workspace_roots: &[(String, PathBuf)]) -> LegacyStatePaths {
        LegacyStatePaths {
            global_sessions: self.state_root.join("sessions.json"),
            session_sidecars: self.state_root.join("session-sidecars.json"),
            task_store: self.state_root.join("task-store.json"),
            workspace_sessions: workspace_roots
                .iter()
                .map(|(workspace_id, root)| {
                    (
                        workspace_id.clone(),
                        root.join(".magi").join("sessions.json"),
                    )
                })
                .collect(),
        }
    }

    fn load_legacy_session_state(
        &self,
        legacy: &LegacyStatePaths,
    ) -> Result<SessionDurableState, DaemonError> {
        let mut merged = if legacy.global_sessions.exists() {
            self.read_legacy_session_file(&legacy.global_sessions)?
        } else {
            SessionDurableState::default()
        };
        let mut session_ids = merged
            .sessions
            .iter()
            .map(|session| session.session_id.clone())
            .collect::<HashSet<_>>();

        for (workspace_id, path) in &legacy.workspace_sessions {
            if !path.exists() {
                continue;
            }
            let state = self.read_legacy_session_file(path)?;
            if let Some(session) = state
                .sessions
                .iter()
                .find(|session| session.workspace_id.as_deref() != Some(workspace_id.as_str()))
            {
                return Err(DaemonError::internal(format!(
                    "旧 workspace session 文件包含错误归属 {}: {}",
                    session.session_id,
                    path.display()
                )));
            }
            if let Some(duplicate) = state
                .sessions
                .iter()
                .find(|session| !session_ids.insert(session.session_id.clone()))
            {
                return Err(DaemonError::internal(format!(
                    "旧布局包含重复 session {}: {}",
                    duplicate.session_id,
                    path.display()
                )));
            }
            if let (Some(current), Some(incoming)) = (
                merged.current_session_id.as_ref(),
                state.current_session_id.as_ref(),
            ) && current != incoming
            {
                return Err(DaemonError::internal(format!(
                    "旧布局包含冲突的 current session: {current} 与 {incoming}"
                )));
            }
            merged.append_state(state);
        }
        Ok(merged)
    }

    fn read_legacy_session_file(&self, path: &Path) -> Result<SessionDurableState, DaemonError> {
        let content = fs::read_to_string(path)?;
        let mut value: serde_json::Value = serde_json::from_str(&content).map_err(|error| {
            DaemonError::internal(format!(
                "解析旧 session 状态失败 {}: {error}",
                path.display()
            ))
        })?;
        migrate_session_goal_state(&mut value);
        serde_json::from_value(value).map_err(|error| {
            DaemonError::internal(format!(
                "迁移旧 session schema 失败 {}: {error}",
                path.display()
            ))
        })
    }

    fn validate_migrated_layout(
        &self,
        workspace_roots: &[(String, PathBuf)],
        expected_sessions: &SessionDurableState,
        expected_task_checkpoint: Option<&serde_json::Value>,
    ) -> Result<(), DaemonError> {
        let verifier = StateRepository::new(self.state_root.clone());
        let (actual_sessions, _) = verifier.load_session_projections(workspace_roots)?;
        let mut expected_ids = expected_sessions
            .sessions
            .iter()
            .map(|session| session.session_id.clone())
            .collect::<Vec<_>>();
        let mut actual_ids = actual_sessions
            .sessions
            .iter()
            .map(|session| session.session_id.clone())
            .collect::<Vec<_>>();
        expected_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        actual_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        if expected_ids != actual_ids
            || expected_sessions.current_session_id != actual_sessions.current_session_id
        {
            return Err(DaemonError::internal(
                "迁移后的 session 索引或 current 指针校验失败".to_string(),
            ));
        }
        for session_id in &expected_ids {
            let expected = expected_sessions.durable_state_for_session(session_id);
            let actual = actual_sessions.durable_state_for_session(session_id);
            if serde_json::to_value(expected).map_err(DaemonError::from)?
                != serde_json::to_value(actual).map_err(DaemonError::from)?
            {
                return Err(DaemonError::internal(format!(
                    "迁移后的 session projection 校验失败: {session_id}"
                )));
            }
        }

        if let Some(expected) = expected_task_checkpoint {
            let restored =
                TaskStore::restore_from_projection_directory(&self.task_store_projection_path())?
                    .ok_or_else(|| DaemonError::internal("迁移后的 task store 为空".to_string()))?;
            if &restored.checkpoint() != expected {
                return Err(DaemonError::internal(
                    "迁移后的 task store checkpoint 校验失败".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn new_layout_exists(&self, workspace_roots: &[(String, PathBuf)]) -> bool {
        self.session_projection_root().exists()
            || self.state_root.join("session-events").exists()
            || self.state_root.join("session-current.json").exists()
            || self.state_root.join("session-app-meta.json").exists()
            || self.task_store_projection_path().exists()
            || workspace_roots.iter().any(|(_, root)| {
                root.join(".magi").join("session-projections").exists()
                    || root
                        .join(".magi")
                        .join("session-workspace-meta.json")
                        .exists()
            })
    }

    fn clear_uncommitted_new_layout(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<(), DaemonError> {
        for path in [
            self.session_projection_root(),
            self.state_root.join("session-events"),
            self.task_store_projection_path(),
        ] {
            self.remove_dir_if_exists(&path)?;
        }
        for path in [
            self.state_root.join("session-current.json"),
            self.state_root.join("session-app-meta.json"),
        ] {
            self.remove_file_if_exists(&path)?;
        }
        for (_, root) in workspace_roots {
            self.remove_dir_if_exists(&root.join(".magi").join("session-projections"))?;
            self.remove_file_if_exists(&root.join(".magi").join("session-workspace-meta.json"))?;
        }
        *self
            .session_projection_cache
            .lock()
            .expect("session projection cache lock poisoned") = SessionProjectionCache::default();
        self.session_event_cache
            .lock()
            .expect("session event cache lock poisoned")
            .clear();
        self.event_accepted_submissions
            .lock()
            .expect("event accepted submission cache lock poisoned")
            .clear();
        Ok(())
    }

    fn archive_legacy_sources(&self, legacy: &LegacyStatePaths) -> Result<(), DaemonError> {
        let archive_root = self
            .state_root
            .join("migrations")
            .join("legacy-v1")
            .join("archive");
        for (source, relative) in [
            (&legacy.global_sessions, PathBuf::from("sessions.json")),
            (
                &legacy.session_sidecars,
                PathBuf::from("session-sidecars.json"),
            ),
            (&legacy.task_store, PathBuf::from("task-store.json")),
        ] {
            if source.exists() {
                Self::archive_legacy_source(source, &archive_root.join(relative))?;
            }
        }
        for (workspace_id, source) in &legacy.workspace_sessions {
            if !source.exists() {
                continue;
            }
            let workspace_dir =
                Self::session_projection_file_name(&magi_core::SessionId::new(workspace_id))
                    .trim_end_matches(".json")
                    .to_string();
            Self::archive_legacy_source(
                source,
                &archive_root
                    .join("workspaces")
                    .join(workspace_dir)
                    .join("sessions.json"),
            )?;
        }
        Ok(())
    }

    fn archive_legacy_source(source: &Path, target: &Path) -> Result<(), DaemonError> {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        magi_core::fs_atomic::write_atomic(target, fs::read(source)?)?;
        Ok(())
    }

    fn remove_legacy_sources(&self, legacy: &LegacyStatePaths) -> Result<(), DaemonError> {
        for path in legacy.existing_paths() {
            self.remove_file_if_exists(&path)?;
        }
        Ok(())
    }

    fn remove_file_if_exists(&self, path: &Path) -> Result<(), DaemonError> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn remove_dir_if_exists(&self, path: &Path) -> Result<(), DaemonError> {
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn read_json_strict<T>(&self, path: &Path) -> Result<T, DaemonError>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        let content = fs::read_to_string(path)?;
        serde_json::from_str(&content).map_err(|error| {
            DaemonError::internal(format!("解析状态文件失败 {}: {error}", path.display()))
        })
    }

    fn session_projection_path(
        &self,
        session_id: &magi_core::SessionId,
        workspace_id: Option<&str>,
        workspace_roots: &HashMap<String, PathBuf>,
    ) -> PathBuf {
        workspace_id
            .and_then(|workspace_id| workspace_roots.get(workspace_id))
            .map(|root| root.join(".magi").join("session-projections"))
            .unwrap_or_else(|| self.session_projection_root())
            .join(Self::session_projection_file_name(session_id))
    }

    fn validate_session_projection(
        snapshot: &SessionProjectionSnapshot,
        path: &Path,
    ) -> Result<magi_core::SessionId, DaemonError> {
        if snapshot.durable.current_session_id.is_some() || snapshot.durable.sessions.len() != 1 {
            return Err(DaemonError::internal(format!(
                "session projection 必须只包含一个 session 且不能包含 current 指针: {}",
                path.display()
            )));
        }
        let session_id = snapshot.durable.sessions[0].session_id.clone();
        let scoped = snapshot
            .durable
            .timeline
            .iter()
            .all(|entry| entry.session_id == session_id)
            && snapshot
                .durable
                .canonical_turns
                .iter()
                .all(|turn| turn.session_id == session_id)
            && snapshot
                .durable
                .notifications
                .iter()
                .all(|notification| notification.session_id.as_ref() == Some(&session_id))
            && snapshot
                .durable
                .goals
                .iter()
                .all(|goal| goal.session_id == session_id)
            && snapshot
                .durable
                .plans
                .iter()
                .all(|plan| plan.session_id == session_id)
            && snapshot
                .durable
                .thread_registry
                .iter()
                .all(|thread| thread.session_id == session_id);
        if !scoped {
            return Err(DaemonError::internal(format!(
                "session projection 混入其他 session 的事实: {}",
                path.display()
            )));
        }
        let thread_ids = snapshot
            .durable
            .thread_registry
            .iter()
            .map(|thread| &thread.thread_id)
            .collect::<HashSet<_>>();
        if snapshot
            .durable
            .thread_context_checkpoints
            .iter()
            .any(|checkpoint| !thread_ids.contains(&checkpoint.thread_id))
        {
            return Err(DaemonError::internal(format!(
                "session projection 包含无归属 thread 的上下文检查点: {}",
                path.display()
            )));
        }
        if let Some(sidecar) = snapshot.sidecar.as_ref() {
            let ownership_matches = sidecar
                .ownership
                .session_id
                .as_ref()
                .is_none_or(|owner| owner == &session_id);
            let chain_matches = sidecar
                .active_execution_chain
                .as_ref()
                .is_none_or(|chain| chain.session_id == session_id);
            if sidecar.session_id != session_id || !ownership_matches || !chain_matches {
                return Err(DaemonError::internal(format!(
                    "session projection 的 sidecar 归属不一致: {}",
                    path.display()
                )));
            }
        }
        Ok(session_id)
    }

    fn validate_cached_canonical_projection(
        cached_turns: &[magi_session_store::CanonicalTurn],
        cached_event_seq: u64,
        replayed: &SessionConversationProjection,
        path: &Path,
    ) -> Result<(), DaemonError> {
        if cached_event_seq == replayed.last_event_seq() {
            let cached = serde_json::to_value(cached_turns).map_err(DaemonError::from)?;
            let authoritative =
                serde_json::to_value(replayed.canonical_turns()).map_err(DaemonError::from)?;
            if cached != authoritative {
                return Err(DaemonError::internal(format!(
                    "session projection 与相同游标的 canonical 事件重放结果不一致: {}",
                    path.display()
                )));
            }
            return Ok(());
        }

        for cached_turn in cached_turns {
            let replayed_turn = replayed
                .canonical_turns()
                .iter()
                .find(|turn| turn.turn_id == cached_turn.turn_id)
                .ok_or_else(|| {
                    DaemonError::internal(format!(
                        "session projection 包含事件日志中不存在的 turn {}: {}",
                        cached_turn.turn_id,
                        path.display()
                    ))
                })?;
            replayed_turn
                .validate_update_from(cached_turn)
                .map_err(|error| {
                    DaemonError::internal(format!(
                        "session projection 不能推进到 canonical 事件结果 {}: {error}",
                        path.display()
                    ))
                })?;
            for cached_item in &cached_turn.items {
                let replayed_item = replayed_turn
                    .items
                    .iter()
                    .find(|item| item.item_id == cached_item.item_id)
                    .ok_or_else(|| {
                        DaemonError::internal(format!(
                            "session projection 包含事件日志中不存在的 item {}: {}",
                            cached_item.item_id,
                            path.display()
                        ))
                    })?;
                replayed_item
                    .validate_update_from(cached_item)
                    .map_err(|error| {
                        DaemonError::internal(format!(
                            "session projection item 不能推进到 canonical 事件结果 {}: {error}",
                            path.display()
                        ))
                    })?;
            }
        }
        Ok(())
    }

    fn save_session_projection_parts(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
        workspace_roots: &HashMap<String, PathBuf>,
        mark_layout: bool,
    ) -> Result<(), DaemonError> {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let mut cache = self
            .session_projection_cache
            .lock()
            .expect("session projection cache lock poisoned");
        let mut event_cache = self
            .session_event_cache
            .lock()
            .expect("session event cache lock poisoned");
        let mut next_cache = cache.clone();
        let mut next_event_cache = event_cache.clone();
        let mut writes = Vec::<SessionProjectionWrite>::new();
        let mut removals = Vec::<SessionProjectionRemoval>::new();
        let mut sidecar_by_session = HashMap::new();
        for sidecar in &sidecars.runtime_sidecars {
            if sidecar_by_session
                .insert(sidecar.session_id.clone(), sidecar.clone())
                .is_some()
            {
                return Err(DaemonError::internal(format!(
                    "session {} 存在重复 sidecar",
                    sidecar.session_id
                )));
            }
        }
        let mut retained_ids = HashSet::new();
        for session in &durable.sessions {
            if !retained_ids.insert(session.session_id.clone()) {
                return Err(DaemonError::internal(format!(
                    "拒绝持久化重复 session projection: {}",
                    session.session_id
                )));
            }
        }
        if let Some(orphan_sidecar_id) = sidecar_by_session
            .keys()
            .find(|session_id| !retained_ids.contains(*session_id))
        {
            return Err(DaemonError::internal(format!(
                "拒绝持久化无 session 归属的 sidecar: {orphan_sidecar_id}"
            )));
        }

        if let Some(current_session_id) = durable.current_session_id.as_ref()
            && !retained_ids.contains(current_session_id)
        {
            return Err(DaemonError::internal(format!(
                "拒绝持久化悬空 session current 指针: {current_session_id}"
            )));
        }

        for session in &durable.sessions {
            let session_id = session.session_id.clone();
            let path = self.session_projection_path(
                &session_id,
                session.workspace_id.as_deref(),
                workspace_roots,
            );
            let previous = cache
                .snapshots
                .get(&session_id)
                .filter(|(previous_path, _)| previous_path == &path)
                .map(|(_, content)| content.clone());
            let mut next_durable = durable.durable_state_for_session(&session_id);
            let event_root = self.session_event_root(&session_id);
            let event_projection = match next_event_cache.get(&session_id) {
                Some(projection) => projection.clone(),
                None => SessionConversationProjection::load(&event_root, &session_id)?,
            };
            let memory_canonical =
                serde_json::to_value(&next_durable.canonical_turns).map_err(DaemonError::from)?;
            let authoritative = serde_json::to_value(event_projection.canonical_turns())
                .map_err(DaemonError::from)?;
            if memory_canonical != authoritative {
                return Err(DaemonError::internal(format!(
                    "session projection 不能反向生成 canonical 事实: {session_id}"
                )));
            }
            next_durable.canonical_turns = event_projection.canonical_turns().to_vec();
            let canonical_event_seq = event_projection.last_event_seq();
            next_event_cache.insert(session_id.clone(), event_projection);
            let snapshot = SessionProjectionSnapshot {
                canonical_event_seq,
                durable: next_durable,
                sidecar: sidecar_by_session.get(&session_id).cloned(),
            };
            Self::validate_session_projection(&snapshot, &path)?;
            let content = serde_json::to_vec_pretty(&snapshot).map_err(DaemonError::from)?;
            let content = String::from_utf8(content).map_err(|error| {
                DaemonError::internal(format!("session projection 不是 UTF-8: {error}"))
            })?;
            if previous.as_deref() != Some(content.as_str()) {
                writes.push(SessionProjectionWrite {
                    path: path.clone(),
                    content: content.clone(),
                });
            }
            if let Some((old_path, _)) = next_cache
                .snapshots
                .insert(session_id, (path.clone(), content))
                && old_path != path
            {
                next_cache.pending_removals.insert(old_path);
            }
        }

        let stale_ids = next_cache
            .snapshots
            .iter()
            .filter(|(session_id, _)| !retained_ids.contains(session_id))
            .map(|(session_id, _)| session_id.clone())
            .collect::<Vec<_>>();
        for session_id in stale_ids {
            if let Some((path, _)) = next_cache.snapshots.remove(&session_id) {
                next_cache.pending_removals.insert(path);
            }
            next_event_cache.remove(&session_id);
            next_cache
                .pending_event_removals
                .insert(self.session_event_root(&session_id));
        }

        let current_path = self.state_root.join("session-current.json");
        let current_content =
            serde_json::to_vec_pretty(&durable.current_session_id).map_err(DaemonError::from)?;
        let current_content = String::from_utf8(current_content).map_err(|error| {
            DaemonError::internal(format!("session current state 不是 UTF-8: {error}"))
        })?;
        if next_cache
            .global
            .as_ref()
            .map(|(_, previous)| previous != &current_content)
            .unwrap_or(true)
        {
            writes.push(SessionProjectionWrite {
                path: current_path.clone(),
                content: current_content.clone(),
            });
        }
        next_cache.global = Some((current_path, current_content));

        let app_meta = SessionDurableState {
            notifications: durable
                .notifications
                .iter()
                .filter(|notification| {
                    matches!(
                        notification.scope,
                        magi_session_store::NotificationScope::App
                    )
                })
                .cloned()
                .collect(),
            ..SessionDurableState::default()
        };
        let app_meta_path = self.state_root.join("session-app-meta.json");
        let app_meta_content = serde_json::to_vec_pretty(&app_meta).map_err(DaemonError::from)?;
        let app_meta_content = String::from_utf8(app_meta_content).map_err(|error| {
            DaemonError::internal(format!("session app meta 不是 UTF-8: {error}"))
        })?;
        if next_cache
            .app_meta
            .as_ref()
            .map(|(_, previous)| previous != &app_meta_content)
            .unwrap_or(true)
        {
            writes.push(SessionProjectionWrite {
                path: app_meta_path.clone(),
                content: app_meta_content.clone(),
            });
        }
        next_cache.app_meta = Some((app_meta_path, app_meta_content));

        for (workspace_id, root) in workspace_roots {
            let meta = SessionDurableState {
                notifications: durable
                    .notifications
                    .iter()
                    .filter(|notification| {
                        notification.workspace_id.as_deref() == Some(workspace_id.as_str())
                            && matches!(
                                notification.scope,
                                magi_session_store::NotificationScope::Workspace
                            )
                    })
                    .cloned()
                    .collect(),
                ..SessionDurableState::default()
            };
            let path = root.join(".magi").join("session-workspace-meta.json");
            let content = serde_json::to_vec_pretty(&meta).map_err(DaemonError::from)?;
            let content = String::from_utf8(content).map_err(|error| {
                DaemonError::internal(format!("workspace meta 不是 UTF-8: {error}"))
            })?;
            if next_cache
                .workspace_meta
                .get(workspace_id)
                .map(|(_, previous)| previous != &content)
                .unwrap_or(true)
            {
                writes.push(SessionProjectionWrite {
                    path: path.clone(),
                    content: content.clone(),
                });
            }
            next_cache
                .workspace_meta
                .insert(workspace_id.clone(), (path, content));
        }

        let pending_removals = next_cache
            .pending_removals
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for path in pending_removals {
            removals.push(SessionProjectionRemoval {
                path,
                kind: SessionProjectionRemovalKind::File,
            });
        }

        let pending_event_removals = next_cache
            .pending_event_removals
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for path in pending_event_removals {
            removals.push(SessionProjectionRemoval {
                path,
                kind: SessionProjectionRemovalKind::Directory,
            });
        }

        // 这里仅收敛旧版本 accepted journal。新请求的 accepted 事实已经和 canonical
        // event 写入同一个 event segment，不再经过该文件。
        let accepted_path = self.accepted_submissions_path();
        let mut accepted = self.read_accepted_submission_journal_strict(&accepted_path)?;
        for record in &mut accepted.records {
            record.session_checkpointed = true;
        }
        accepted
            .records
            .retain(|record| !(record.session_checkpointed && record.task_checkpointed));
        if accepted.records.is_empty() {
            if accepted_path.exists() {
                removals.push(SessionProjectionRemoval {
                    path: accepted_path,
                    kind: SessionProjectionRemovalKind::File,
                });
            }
        } else {
            let content = serde_json::to_string_pretty(&accepted).map_err(DaemonError::from)?;
            writes.push(SessionProjectionWrite {
                path: accepted_path,
                content,
            });
        }

        let transaction = SessionProjectionTransaction {
            schema_version: SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION,
            transaction_id: format!("session-projection-{}", magi_core::UtcMillis::now().0),
            writes,
            removals,
        };
        self.commit_session_projection_transaction_locked(&transaction, workspace_roots)?;
        if mark_layout {
            self.ensure_state_layout_marker_locked()?;
        }

        next_cache.pending_removals.clear();
        next_cache.pending_event_removals.clear();
        *cache = next_cache;
        *event_cache = next_event_cache;

        Ok(())
    }

    fn ensure_state_layout_marker_locked(&self) -> Result<(), DaemonError> {
        let layout_path = self.state_root.join("state-layout.json");
        if layout_path.exists() {
            let marker: StateLayoutMarker = self.read_json_strict(&layout_path)?;
            if marker.version != STATE_LAYOUT_VERSION {
                return Err(DaemonError::internal(format!(
                    "不支持的 state layout 版本: {}",
                    marker.version
                )));
            }
            return Ok(());
        }
        if self.state_root.join("state-layout-migration.json").exists() {
            return Err(DaemonError::internal(
                "state layout migration 尚未完成，拒绝提交 v2 状态".to_string(),
            ));
        }
        self.write_json_atomically_locked(
            layout_path,
            &StateLayoutMarker {
                version: STATE_LAYOUT_VERSION,
            },
        )
    }

    fn session_projection_file_name(session_id: &magi_core::SessionId) -> String {
        let mut encoded = String::new();
        for byte in session_id.as_str().bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                encoded.push(byte as char);
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
        format!("{encoded}.json")
    }

    pub(crate) fn accepted_submissions_path(&self) -> PathBuf {
        self.state_root.join("accepted-submissions.json")
    }

    pub(crate) fn task_store_projection_path(&self) -> PathBuf {
        self.state_root.join("task-store-projections")
    }

    pub(crate) fn load_accepted_submissions(
        &self,
    ) -> Result<Vec<AcceptedSubmissionRecord>, DaemonError> {
        let path = self.accepted_submissions_path();
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let committed_generation =
            TaskStore::committed_projection_generation(&self.task_store_projection_path())?
                .unwrap_or(0);
        let mut journal = self.read_accepted_submission_journal_strict(&path)?;
        let event_records = self
            .event_accepted_submissions
            .lock()
            .expect("event accepted submission cache lock poisoned")
            .clone();
        let mut records = Vec::with_capacity(event_records.len() + journal.records.len());
        let mut seen = HashSet::new();
        for record in event_records.into_iter().chain(journal.records.drain(..)) {
            let key = format!(
                "{}\u{0}{}",
                record.session.session.session_id, record.session.canonical_turn.turn_id
            );
            if seen.insert(key) {
                records.push(record);
            }
        }
        let mut reconciled = false;
        for record in &mut records {
            if !record.task_checkpointed
                && (committed_generation > record.task_projection_generation_at_acceptance
                    || TaskStore::committed_projection_contains_root(
                        &self.task_store_projection_path(),
                        &record.task.root_task_id,
                    )?)
            {
                record.task_checkpointed = true;
                reconciled = true;
            }
        }
        if reconciled {
            let mut legacy_journal = self.read_accepted_submission_journal_strict(&path)?;
            for record in &records {
                if record.task_checkpointed {
                    if let Some(legacy) = legacy_journal.records.iter_mut().find(|legacy| {
                        legacy.session.session.session_id == record.session.session.session_id
                            && legacy.session.canonical_turn.turn_id
                                == record.session.canonical_turn.turn_id
                    }) {
                        legacy.task_checkpointed = true;
                    }
                }
            }
            self.finish_accepted_submission_journal_locked(path, legacy_journal)?;
        }
        records.retain(|record| !(record.session_checkpointed && record.task_checkpointed));
        Ok(records)
    }

    #[cfg(test)]
    pub(crate) fn save_accepted_submission(
        &self,
        session_store: &SessionStore,
        session_id: &magi_core::SessionId,
        turn_id: &str,
        task_store: &TaskStore,
        root_task_id: &TaskId,
    ) -> Result<(), DaemonError> {
        let session = session_store
            .session_acceptance_record(session_id, turn_id)
            .ok_or_else(|| DaemonError::internal("构造 accepted session journal 失败"))?;
        let task = task_store
            .get_task(root_task_id)
            .ok_or_else(|| DaemonError::internal("构造 accepted task journal 失败"))?;
        self.save_accepted_submission_record(session_id, &session, &task)
    }

    #[cfg(test)]
    fn save_accepted_submission_record(
        &self,
        session_id: &SessionId,
        session: &SessionAcceptanceRecord,
        task: &Task,
    ) -> Result<(), DaemonError> {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        self.save_accepted_submission_record_locked(session_id, session, task)
    }

    #[cfg(test)]
    fn save_accepted_submission_record_locked(
        &self,
        session_id: &SessionId,
        session: &SessionAcceptanceRecord,
        task: &Task,
    ) -> Result<(), DaemonError> {
        if session.session.session_id != *session_id
            || session.canonical_turn.session_id != *session_id
        {
            return Err(DaemonError::internal(format!(
                "accepted session journal 归属不一致: {}",
                session_id
            )));
        }
        let path = self.accepted_submissions_path();
        let task_projection_generation_at_acceptance =
            TaskStore::committed_projection_generation(&self.task_store_projection_path())?
                .unwrap_or(0);
        let mut journal = self.read_accepted_submission_journal_strict(&path)?;
        journal.records.retain(|record| {
            !(record.session.session.session_id == *session_id
                && record.session.canonical_turn.turn_id == session.canonical_turn.turn_id)
        });
        journal.records.push(AcceptedSubmissionRecord {
            session: session.clone(),
            task: task.clone(),
            session_checkpointed: false,
            task_checkpointed: false,
            task_projection_generation_at_acceptance,
        });
        journal.records.sort_by(|left, right| {
            left.session
                .canonical_turn
                .accepted_at
                .0
                .cmp(&right.session.canonical_turn.accepted_at.0)
                .then_with(|| {
                    left.session
                        .canonical_turn
                        .turn_id
                        .cmp(&right.session.canonical_turn.turn_id)
                })
        });
        self.write_json_atomically_locked(path, &journal)
    }

    /// 在同一个 repository 写锁内提交 task manifest 并收敛 accepted WAL。
    /// manifest 是 task projection 的唯一提交点；提交后所有更早接纳的 task（包括已删除
    /// 的 task）都已被该完整快照覆盖，因此不得再按 task 是否仍存在来决定 WAL 状态。
    pub(crate) fn checkpoint_task_store(
        &self,
        task_store: &TaskStore,
    ) -> Result<usize, DaemonError> {
        let snapshot = task_store.snapshot();
        self.checkpoint_task_store_snapshot(&snapshot)
    }

    pub(crate) fn checkpoint_task_store_snapshot(
        &self,
        snapshot: &TaskStoreSnapshot,
    ) -> Result<usize, DaemonError> {
        self.checkpoint_task_store_snapshot_inner(snapshot, true)
    }

    fn checkpoint_task_store_snapshot_inner(
        &self,
        snapshot: &TaskStoreSnapshot,
        mark_layout: bool,
    ) -> Result<usize, DaemonError> {
        let path = self.accepted_submissions_path();
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let mut journal = self.read_accepted_submission_journal_strict(&path)?;
        if mark_layout {
            self.ensure_state_layout_marker_locked()?;
        }
        let projection_count = TaskStore::checkpoint_snapshot_to_projection_directory(
            snapshot,
            &self.task_store_projection_path(),
        )?;
        for record in &mut journal.records {
            record.task_checkpointed = true;
        }
        if let Err(error) = self.finish_accepted_submission_journal_locked(path, journal) {
            // task projection 的 manifest 已经是提交点。WAL 收尾只是清理动作，
            // 失败时保留 WAL，下一次启动会依据已提交 manifest 重新收敛，不能把
            // 已经 durable 的 task mutation 伪装成失败并阻止内存提交。
            warn!(
                ?error,
                "task checkpoint 已提交，但 accepted WAL 收尾失败，将在下次恢复时重试"
            );
        }
        Ok(projection_count)
    }

    pub(crate) fn prune_accepted_submissions(&self) -> Result<(), DaemonError> {
        let path = self.accepted_submissions_path();
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let journal = self.read_accepted_submission_journal_strict(&path)?;
        self.finish_accepted_submission_journal_locked(path, journal)
    }

    fn read_accepted_submission_journal_strict(
        &self,
        path: &Path,
    ) -> Result<AcceptedSubmissionJournal, DaemonError> {
        if !path.exists() {
            return Ok(AcceptedSubmissionJournal::default());
        }
        let content = fs::read_to_string(path)?;
        let journal: AcceptedSubmissionJournal =
            serde_json::from_str(&content).map_err(|error| {
                DaemonError::internal(format!(
                    "accepted submission journal 损坏，拒绝继续启动或覆盖 {}: {error}",
                    path.display()
                ))
            })?;
        if journal.schema_version != ACCEPTED_SUBMISSION_JOURNAL_SCHEMA_VERSION {
            return Err(DaemonError::internal(format!(
                "accepted submission journal schemaVersion 不受支持 {}: {}",
                path.display(),
                journal.schema_version
            )));
        }
        Ok(journal)
    }

    fn finish_accepted_submission_journal_locked(
        &self,
        path: PathBuf,
        mut journal: AcceptedSubmissionJournal,
    ) -> Result<(), DaemonError> {
        journal
            .records
            .retain(|record| !(record.session_checkpointed && record.task_checkpointed));
        if journal.records.is_empty() {
            Self::remove_file_durable(&path)?;
        } else {
            self.write_json_atomically_locked(path, &journal)?;
        }
        Ok(())
    }

    pub(crate) fn session_projection_root(&self) -> PathBuf {
        self.state_root.join("session-projections")
    }

    pub(crate) fn session_event_root(&self, session_id: &magi_core::SessionId) -> PathBuf {
        self.state_root
            .join("session-events")
            .join(Self::session_projection_file_name(session_id).trim_end_matches(".json"))
    }

    pub(crate) fn canonical_event_next_sequence(
        &self,
        session_id: &magi_core::SessionId,
    ) -> Result<u64, DaemonError> {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let mut cache = self
            .session_event_cache
            .lock()
            .expect("session event cache lock poisoned");
        let event_root = self.session_event_root(session_id);
        let projection = match cache.get(session_id) {
            Some(projection) => projection.clone(),
            None => SessionConversationProjection::load(&event_root, session_id)?,
        };
        let next_sequence = projection.last_event_seq().saturating_add(1).max(1);
        cache.insert(session_id.clone(), projection);
        Ok(next_sequence)
    }

    pub(crate) fn load_workspace_durable_state(
        &self,
    ) -> Result<WorkspaceDurableState, DaemonError> {
        self.read_json_or_default(self.state_root.join("workspaces.json"))
    }

    pub(crate) fn save_workspace_durable_state(
        &self,
        state: &WorkspaceDurableState,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.state_root.join("workspaces.json"), state)
    }

    pub(crate) fn worker_runtime_snapshot_path(&self) -> PathBuf {
        self.state_root.join("worker-runtime.json")
    }

    pub(crate) fn load_worker_runtime_snapshot(
        &self,
    ) -> Result<WorkerRuntimeDurableSnapshot, DaemonError> {
        self.read_json_or_default(self.worker_runtime_snapshot_path())
    }

    pub(crate) fn save_worker_runtime_snapshot(
        &self,
        snapshot: &WorkerRuntimeDurableSnapshot,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.worker_runtime_snapshot_path(), snapshot)
    }

    pub(crate) fn workspace_recovery_sidecars_path(&self) -> PathBuf {
        self.state_root.join("workspace-recovery-sidecars.json")
    }

    pub(crate) fn load_workspace_recovery_sidecars(
        &self,
    ) -> Result<WorkspaceRecoverySidecarStoreState, DaemonError> {
        self.read_json_or_default(self.workspace_recovery_sidecars_path())
    }

    pub(crate) fn save_workspace_recovery_sidecars(
        &self,
        state: &WorkspaceRecoverySidecarStoreState,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.workspace_recovery_sidecars_path(), state)
    }

    pub(crate) fn audit_usage_ledger_path(&self) -> PathBuf {
        self.state_root.join("audit-usage-ledger.json")
    }

    pub(crate) fn load_audit_usage_ledger(&self) -> Result<AuditUsageLedgerSnapshot, DaemonError> {
        self.read_json_or_default(self.audit_usage_ledger_path())
    }

    pub(crate) fn knowledge_state_path(&self) -> PathBuf {
        self.state_root.join("knowledge.json")
    }

    pub(crate) fn load_knowledge_state(&self) -> Result<KnowledgeState, DaemonError> {
        self.read_json_or_default(self.knowledge_state_path())
    }

    fn read_json_or_default<T>(&self, path: PathBuf) -> Result<T, DaemonError>
    where
        T: Default + for<'de> serde::Deserialize<'de>,
    {
        if !path.exists() {
            return Ok(T::default());
        }
        let content = fs::read_to_string(&path)?;
        serde_json::from_str(&content).map_err(|error| {
            DaemonError::internal(format!(
                "持久状态文件损坏，拒绝回退到空状态 {}: {error}",
                path.display()
            ))
        })
    }

    fn write_json_atomically<T>(&self, path: PathBuf, value: &T) -> Result<(), DaemonError>
    where
        T: serde::Serialize,
    {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        self.write_json_atomically_locked(path, value)
    }

    fn write_json_atomically_locked<T>(&self, path: PathBuf, value: &T) -> Result<(), DaemonError>
    where
        T: serde::Serialize,
    {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_vec_pretty(value)?;
        magi_core::fs_atomic::write_atomic(&path, content)?;
        Ok(())
    }
}

impl CanonicalTurnEventWriter for StateRepository {
    fn append_canonical_turn_transaction(
        &self,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
    ) -> DomainResult<()> {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        self.append_canonical_turn_transaction_locked(session_id, mutations, None)
    }

    fn append_canonical_turn_transaction_with_acceptance(
        &self,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
        acceptance: &SessionAcceptanceRecord,
        task: &Task,
    ) -> DomainResult<()> {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");

        let Some(last_mutation) = mutations.last() else {
            return Err(DomainError::InvalidState {
                message: "accepted canonical event transaction 不能为空".to_string(),
            });
        };
        let mut expected_canonical_turn = acceptance.canonical_turn.clone();
        let mut committed_canonical_turn = last_mutation.next.clone();
        expected_canonical_turn.normalize();
        committed_canonical_turn.normalize();
        if expected_canonical_turn != committed_canonical_turn {
            return Err(DomainError::InvalidState {
                message: format!(
                    "accepted journal canonical turn 与事件事务不一致: {}",
                    acceptance.canonical_turn.turn_id
                ),
            });
        }

        let task_projection_generation_at_acceptance =
            TaskStore::committed_projection_generation(&self.task_store_projection_path())
                .map_err(|error| DomainError::Persistence {
                    message: error.to_string(),
                })?
                .unwrap_or(0);
        let acceptance = AcceptedSubmissionRecord {
            session: acceptance.clone(),
            task: task.clone(),
            // canonical event 已经包含 session、timeline 和 sidecar，事件 segment 本身
            // 就是 session accepted 事实的 durable 提交点。
            session_checkpointed: true,
            task_checkpointed: false,
            task_projection_generation_at_acceptance,
        };
        self.append_canonical_turn_transaction_locked(session_id, mutations, Some(acceptance))
    }
}

impl StateRepository {
    fn append_canonical_turn_transaction_locked(
        &self,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
        acceptance: Option<AcceptedSubmissionRecord>,
    ) -> DomainResult<()> {
        let mut cache = self
            .session_event_cache
            .lock()
            .expect("session event cache lock poisoned");
        let event_root = self.session_event_root(session_id);
        let projection =
            match cache.get(session_id) {
                Some(projection) => projection.clone(),
                None => SessionConversationProjection::load(&event_root, session_id).map_err(
                    |error| DomainError::Persistence {
                        message: error.to_string(),
                    },
                )?,
            };
        let next = match acceptance.as_ref() {
            Some(acceptance) => projection.append_transaction_with_acceptance(
                &event_root,
                session_id,
                mutations,
                acceptance.clone(),
            ),
            None => projection.append_transaction(&event_root, session_id, mutations),
        }
        .map_err(|error| DomainError::Persistence {
            message: error.to_string(),
        })?;
        cache.insert(session_id.clone(), next);
        if let Some(acceptance) = acceptance {
            let mut accepted = self
                .event_accepted_submissions
                .lock()
                .expect("event accepted submission cache lock poisoned");
            accepted.retain(|existing| {
                !(existing.session.session.session_id == acceptance.session.session.session_id
                    && existing.session.canonical_turn.turn_id
                        == acceptance.session.canonical_turn.turn_id)
            });
            accepted.push(acceptance);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RuntimeSidecarFlushReport {
    pub(crate) session_sidecars_flushed: bool,
    pub(crate) workspace_recovery_sidecars_flushed: bool,
    pub(crate) worker_runtime_snapshot_flushed: bool,
}

#[derive(Clone)]
pub(crate) struct RuntimeSidecarPersistence {
    state_repository: StateRepository,
    session_store: Arc<SessionStore>,
    workspace_store: Arc<WorkspaceStore>,
    worker_runtime: WorkerRuntime,
}

impl RuntimeSidecarPersistence {
    pub(crate) fn new(
        state_repository: StateRepository,
        session_store: Arc<SessionStore>,
        workspace_store: Arc<WorkspaceStore>,
        worker_runtime: WorkerRuntime,
    ) -> Self {
        Self {
            state_repository,
            session_store,
            workspace_store,
            worker_runtime,
        }
    }

    pub(crate) fn worker_runtime_snapshot_dirty(&self) -> bool {
        self.worker_runtime.durable_snapshot_dirty()
    }

    fn persist_session_snapshot(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
    ) -> Result<(), DaemonError> {
        self.state_repository
            .save_session_projection_state(durable, sidecars)?;
        Ok(())
    }

    fn save_session_projection(&self) -> Result<(), DaemonError> {
        self.session_store
            .persist_projection_with(|durable, sidecars| {
                self.persist_session_snapshot(durable, sidecars)
            })
    }

    pub(crate) fn flush_runtime_sidecars(&self) -> Result<RuntimeSidecarFlushReport, DaemonError> {
        let worker_runtime_snapshot_flushed =
            self.worker_runtime
                .flush_durable_snapshot_with(|snapshot| {
                    self.state_repository.save_worker_runtime_snapshot(snapshot)
                })?;
        let session_sidecars_flushed =
            self.session_store
                .flush_execution_sidecars_with(|durable, sidecars| {
                    self.persist_session_snapshot(durable, sidecars)
                })?;
        if let Err(error) = self.state_repository.prune_accepted_submissions() {
            warn!(?error, "刷新运行时 sidecar 后清理 accepted journal 失败");
        }
        let workspace_recovery_sidecars_flushed =
            self.workspace_store.flush_recovery_sidecars_with(|state| {
                self.state_repository
                    .save_workspace_recovery_sidecars(state)
            })?;
        Ok(RuntimeSidecarFlushReport {
            session_sidecars_flushed,
            workspace_recovery_sidecars_flushed,
            worker_runtime_snapshot_flushed,
        })
    }

    pub(crate) fn persist_session_checkpoint(&self) -> Result<bool, DaemonError> {
        let sidecars_flushed =
            self.session_store
                .flush_execution_sidecars_with(|durable, state| {
                    self.persist_session_snapshot(durable, state)
                })?;
        if !sidecars_flushed {
            self.save_session_projection()?;
        }
        Ok(sidecars_flushed)
    }
}

/// 仅用于 v1 -> v2 一次性迁移；v2 运行时不再接受这些旧 goal 字段。
fn migrate_session_goal_state(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object
        .entry("current_session_id")
        .or_insert(serde_json::Value::Null);
    for key in [
        "sessions",
        "timeline",
        "canonical_turns",
        "notifications",
        "goals",
        "thread_registry",
        "thread_context_checkpoints",
    ] {
        object
            .entry(key)
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    }
    if !object.contains_key("plans") {
        let plans = object
            .remove("todo_lists")
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
        object.insert("plans".to_string(), plans);
    } else {
        object.remove("todo_lists");
    }
    let latest = {
        let Some(goals) = object
            .get_mut("goals")
            .and_then(serde_json::Value::as_array_mut)
        else {
            return;
        };
        let mut latest = HashMap::<String, (u64, String, bool, bool)>::new();
        for goal in goals.iter_mut() {
            let Some(goal) = goal.as_object_mut() else {
                continue;
            };
            goal.remove("consecutiveFailureTurns");
            let Some(session_id) = goal.get("sessionId").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some(goal_id) = goal.get("goalId").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let updated_at = goal
                .get("updatedAt")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            let status = goal
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let cleared = status == "cleared";
            let unfinished = matches!(
                status,
                "active" | "paused" | "blocked" | "usage_limited" | "budget_limited"
            );
            let entry = latest.entry(session_id.to_string()).or_insert((
                updated_at,
                goal_id.to_string(),
                cleared,
                unfinished,
            ));
            if updated_at > entry.0 || (updated_at == entry.0 && goal_id > entry.1.as_str()) {
                *entry = (updated_at, goal_id.to_string(), cleared, unfinished);
            }
        }
        goals.retain(|goal| {
            let Some(goal) = goal.as_object() else {
                return false;
            };
            let Some(session_id) = goal.get("sessionId").and_then(serde_json::Value::as_str) else {
                return false;
            };
            let Some(goal_id) = goal.get("goalId").and_then(serde_json::Value::as_str) else {
                return false;
            };
            latest
                .get(session_id)
                .is_some_and(|(_, latest_goal_id, cleared, _)| {
                    !cleared && latest_goal_id == goal_id
                })
        });
        latest
    };

    for plans_key in ["plans"] {
        let Some(plans) = object
            .get_mut(plans_key)
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        plans.retain_mut(|plan| {
            let Some(plan) = plan.as_object_mut() else {
                return false;
            };
            let Some(session_id) = plan.get("sessionId").and_then(serde_json::Value::as_str) else {
                return false;
            };
            let Some((_, goal_id, cleared, unfinished)) = latest.get(session_id) else {
                return true;
            };
            if *cleared {
                return false;
            }
            if *unfinished || plan.get("goalId").is_some() {
                plan.insert(
                    "goalId".to_string(),
                    serde_json::Value::String(goal_id.clone()),
                );
            }
            true
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{
        AbsolutePath, MissionId, PlanId, PlanItem, PlanItemId, PlanItemStatus, PlanState,
        SessionId, SessionLifecycleStatus, TaskId, TaskKind, TaskStatus, ThreadId, UtcMillis,
        WorkerId, WorkspaceId,
    };
    use magi_session_store::{
        ActiveExecutionChain, ActiveExecutionDispatchContext, ActiveExecutionTurn,
        CanonicalTurnEventWriter, CanonicalTurnMutation, ExecutionThread, ExecutionThreadStatus,
        NotificationRecord, NotificationScope, SessionDurableState, SessionPlan, SessionRecord,
        ThreadChatMessage, ThreadContextCheckpoint, TimelineEntry, TimelineEntryKind,
    };
    use std::{collections::HashMap, thread};

    fn accepted_session_store(
        session_id: &str,
        workspace_id: Option<&str>,
        accepted_at: u64,
    ) -> (SessionStore, String, TaskId) {
        let session_store = SessionStore::new();
        let session_id = SessionId::new(session_id);
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "accepted journal test",
                workspace_id.map(str::to_string),
            )
            .expect("accepted journal test session should be created");
        let turn_id = format!("turn-{session_id}-{accepted_at}");
        let entry_id = format!("timeline-{session_id}-{accepted_at}");
        let task_id = TaskId::new(format!("task-{session_id}-{accepted_at}"));
        let chain = ActiveExecutionChain {
            session_id: session_id.clone(),
            mission_id: MissionId::new(format!("mission-{session_id}")),
            root_task_id: task_id.clone(),
            execution_chain_ref: format!("chain-{session_id}-{accepted_at}"),
            workspace_id: workspace_id.map(WorkspaceId::new),
            active_branch_task_ids: vec![task_id.clone()],
            active_worker_bindings: vec![WorkerId::new(format!("worker-{session_id}"))],
            branches: Vec::new(),
            recovery_ref: None,
            dispatch_context: ActiveExecutionDispatchContext {
                accepted_at: UtcMillis(accepted_at),
                entry_id: entry_id.clone(),
                trimmed_text: Some("test accepted journal".to_string()),
                skill_name: None,
            },
            current_turn: Some(ActiveExecutionTurn {
                turn_id: turn_id.clone(),
                turn_seq: accepted_at,
                accepted_at: UtcMillis(accepted_at),
                completed_at: None,
                status: "accepted".to_string(),
                user_message: Some("test accepted journal".to_string()),
                items: Vec::new(),
            }),
        };
        session_store
            .accept_active_execution_chain_with_timeline_entry(
                session_id,
                magi_session_store::TimelineEntryInput::new(
                    entry_id,
                    TimelineEntryKind::UserMessage,
                    "test accepted journal",
                    UtcMillis(accepted_at),
                ),
                chain,
            )
            .expect("accepted journal test turn should be accepted");
        (session_store, turn_id, task_id)
    }

    fn install_test_event_authority(repository: &StateRepository, session_store: &SessionStore) {
        repository
            .initialize_session_events(&session_store.durable_state())
            .expect("test canonical events should initialize");
        session_store.install_canonical_event_writer(Arc::new(repository.clone()));
    }

    fn accepted_task(task_id: TaskId, accepted_at: u64) -> Task {
        let mission_id = MissionId::new(format!("mission-{task_id}"));
        Task {
            task_id: task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: task_id,
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "accepted journal task".to_string(),
            goal: "accepted journal task".to_string(),
            status: TaskStatus::Pending,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: UtcMillis(accepted_at),
            updated_at: UtcMillis(accepted_at),
        }
    }

    #[test]
    fn accepted_journal_is_retained_until_all_recovery_facts_are_durable() {
        let state_root = unique_temp_dir("magi-accepted-journal-retention");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, turn_id, task_id) =
            accepted_session_store("accepted-journal-retention", None, 10);
        install_test_event_authority(&repository, &session_store);
        let task_store = TaskStore::new();
        task_store
            .insert_task_without_checkpoint(accepted_task(task_id.clone(), 10))
            .expect("accepted task should insert");

        repository
            .save_accepted_submission(
                &session_store,
                &SessionId::new("accepted-journal-retention"),
                &turn_id,
                &task_store,
                &task_id,
            )
            .expect("accepted journal should save");
        repository
            .prune_accepted_submissions()
            .expect("retention check should succeed before checkpoints");
        assert!(repository.accepted_submissions_path().exists());

        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("session projection should save");
        repository
            .checkpoint_task_store(&task_store)
            .expect("task checkpoint should save");
        assert!(!repository.accepted_submissions_path().exists());

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn workspace_accepted_journal_uses_workspace_session_checkpoint() {
        let state_root = unique_temp_dir("magi-accepted-journal-workspace");
        let workspace_root = unique_temp_dir("magi-accepted-journal-workspace-root");
        let repository = StateRepository::new(state_root.clone());
        let workspace_store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-accepted-journal");
        workspace_store
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
            )
            .expect("workspace should register");
        let (session_store, turn_id, task_id) = accepted_session_store(
            "accepted-journal-workspace",
            Some(workspace_id.as_str()),
            20,
        );
        install_test_event_authority(&repository, &session_store);
        let session_id = SessionId::new("accepted-journal-workspace");
        let task_store = TaskStore::new();
        task_store
            .insert_task_without_checkpoint(accepted_task(task_id.clone(), 20))
            .expect("accepted task should insert");

        repository
            .save_accepted_submission(&session_store, &session_id, &turn_id, &task_store, &task_id)
            .expect("workspace accepted journal should save");
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("workspace session projection should save");
        repository
            .checkpoint_task_store(&task_store)
            .expect("workspace task checkpoint should save");
        assert!(!repository.accepted_submissions_path().exists());

        let _ = fs::remove_dir_all(state_root);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn committed_task_manifest_checkpoints_deleted_wal_task_without_resurrection() {
        let state_root = unique_temp_dir("magi-accepted-deleted-task");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, turn_id, task_id) =
            accepted_session_store("accepted-deleted-task", None, 25);
        let session_id = SessionId::new("accepted-deleted-task");
        let task_store = TaskStore::new();
        install_test_event_authority(&repository, &session_store);
        task_store
            .insert_task_without_checkpoint(accepted_task(task_id.clone(), 25))
            .expect("accepted task should insert");
        repository
            .save_accepted_submission(&session_store, &session_id, &turn_id, &task_store, &task_id)
            .expect("accepted WAL should save");
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("session side should checkpoint");
        task_store
            .remove_task(&task_id)
            .expect("task delete should mutate the in-memory snapshot")
            .expect("accepted task should exist before deletion");

        task_store
            .checkpoint_to_projection_directory(&repository.task_store_projection_path())
            .expect("empty task manifest should commit");

        let recovered = StateRepository::new(state_root.clone())
            .load_accepted_submissions()
            .expect("manifest generation should reconcile WAL after a crash");
        assert!(recovered.is_empty());
        assert!(!repository.accepted_submissions_path().exists());
        let restored =
            TaskStore::restore_from_projection_directory(&repository.task_store_projection_path())
                .expect("empty committed task store should restore")
                .expect("manifest should represent an initialized empty store");
        assert!(restored.get_task(&task_id).is_none());

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn accepted_task_after_current_manifest_remains_replayable() {
        let state_root = unique_temp_dir("magi-accepted-after-manifest");
        let repository = StateRepository::new(state_root.clone());
        let task_store = TaskStore::new();
        task_store
            .checkpoint_to_projection_directory(&repository.task_store_projection_path())
            .expect("baseline manifest should commit");
        let (session_store, turn_id, task_id) =
            accepted_session_store("accepted-after-manifest", None, 26);
        let session_id = SessionId::new("accepted-after-manifest");
        task_store
            .insert_task_without_checkpoint(accepted_task(task_id.clone(), 26))
            .expect("accepted task should insert");
        repository
            .save_accepted_submission(&session_store, &session_id, &turn_id, &task_store, &task_id)
            .expect("accepted WAL should save after baseline manifest");

        let recovered = StateRepository::new(state_root.clone())
            .load_accepted_submissions()
            .expect("WAL should load");
        assert_eq!(recovered.len(), 1);
        assert!(!recovered[0].task_checkpointed);
        assert_eq!(recovered[0].task.task_id, task_id);

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn accepted_wal_written_after_manifest_does_not_replay_an_already_projected_task() {
        let state_root = unique_temp_dir("magi-accepted-after-including-manifest");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, turn_id, task_id) =
            accepted_session_store("accepted-after-including-manifest", None, 27);
        let session_id = SessionId::new("accepted-after-including-manifest");
        let task_store = TaskStore::new();
        task_store
            .insert_task_without_checkpoint(accepted_task(task_id.clone(), 27))
            .expect("accepted task should insert");

        repository
            .checkpoint_task_store(&task_store)
            .expect("manifest containing accepted task should commit first");
        repository
            .save_accepted_submission(&session_store, &session_id, &turn_id, &task_store, &task_id)
            .expect("late accepted WAL should save at the committed generation");

        let recovered = StateRepository::new(state_root.clone())
            .load_accepted_submissions()
            .expect("late WAL should reconcile against persisted task membership");
        assert_eq!(recovered.len(), 1);
        assert!(recovered[0].task_checkpointed);
        assert_eq!(recovered[0].task.task_id, task_id);

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn concurrent_accepted_journal_writes_keep_both_records() {
        let state_root = unique_temp_dir("magi-accepted-journal-concurrent");
        let repository = StateRepository::new(state_root.clone());
        let (session_store_a, turn_id_a, task_id_a) =
            accepted_session_store("accepted-journal-concurrent-a", None, 30);
        let (session_store_b, turn_id_b, task_id_b) =
            accepted_session_store("accepted-journal-concurrent-b", None, 31);
        let task_store_a = TaskStore::new();
        task_store_a
            .insert_task_without_checkpoint(accepted_task(task_id_a.clone(), 30))
            .expect("accepted task A should insert");
        let task_store_b = TaskStore::new();
        task_store_b
            .insert_task_without_checkpoint(accepted_task(task_id_b.clone(), 31))
            .expect("accepted task B should insert");
        let repository_a = repository.clone();
        let repository_b = repository.clone();

        thread::scope(|scope| {
            let first = scope.spawn(move || {
                repository_a.save_accepted_submission(
                    &session_store_a,
                    &SessionId::new("accepted-journal-concurrent-a"),
                    &turn_id_a,
                    &task_store_a,
                    &task_id_a,
                )
            });
            let second = scope.spawn(move || {
                repository_b.save_accepted_submission(
                    &session_store_b,
                    &SessionId::new("accepted-journal-concurrent-b"),
                    &turn_id_b,
                    &task_store_b,
                    &task_id_b,
                )
            });
            first
                .join()
                .expect("first journal write should not panic")
                .expect("first journal write should succeed");
            second
                .join()
                .expect("second journal write should not panic")
                .expect("second journal write should succeed");
        });

        let records = repository
            .load_accepted_submissions()
            .expect("accepted journal should load");
        assert_eq!(records.len(), 2, "并发 accepted 写入不能丢失任一恢复记录");

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn corrupted_accepted_journal_is_rejected_without_backup_or_empty_fallback() {
        let state_root = unique_temp_dir("magi-accepted-journal-corrupted");
        let repository = StateRepository::new(state_root.clone());
        let path = repository.accepted_submissions_path();
        fs::write(&path, br#"{"records":["#).expect("corrupted journal should write");

        let error = repository
            .load_accepted_submissions()
            .expect_err("corrupted accepted journal must reject recovery");
        assert!(error.to_string().contains("journal 损坏"));
        assert!(path.exists(), "损坏 journal 必须原地保留以便诊断和恢复");
        assert!(
            !path
                .with_file_name("accepted-submissions.json.stale")
                .exists()
        );

        fs::write(&path, b"{}").expect("missing journal schema fixture should write");
        repository
            .load_accepted_submissions()
            .expect_err("缺少 records 的 journal 不能静默解释为空状态");

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn corrupted_durable_state_is_rejected_without_empty_fallback() {
        let state_root = unique_temp_dir("magi-durable-state-corrupted");
        let repository = StateRepository::new(state_root.clone());
        let path = state_root.join("workspaces.json");
        fs::write(&path, b"{not-json").expect("corrupted durable state should write");

        let error = repository
            .load_workspace_durable_state()
            .expect_err("corrupted durable state must reject recovery");
        assert!(error.to_string().contains("拒绝回退到空状态"));
        assert!(path.exists(), "损坏状态必须原地保留以便诊断和恢复");
        assert!(!path.with_file_name("workspaces.json.stale").exists());

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn duplicate_session_projection_is_deduplicated_only_when_content_matches() {
        let state_root = unique_temp_dir("magi-session-projection-duplicate");
        let duplicate_root = unique_temp_dir("magi-session-projection-duplicate-root");
        let repository = StateRepository::new(state_root.clone());
        let workspace_store = WorkspaceStore::new();
        workspace_store
            .register(
                WorkspaceId::new("duplicate-root"),
                AbsolutePath::new(duplicate_root.to_string_lossy().to_string()),
            )
            .expect("duplicate workspace should register");
        repository
            .save_workspace_durable_state(&workspace_store.durable_state())
            .expect("duplicate workspace state should save");
        let session_store = SessionStore::new();
        let session_id = SessionId::new("session-projection-duplicate");
        session_store
            .create_session(session_id.clone(), "duplicate projection")
            .expect("session should create");
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("initial projection should save");

        let file_name = StateRepository::session_projection_file_name(&session_id);
        let canonical_path = repository.session_projection_root().join(&file_name);
        let duplicate_path = duplicate_root
            .join(".magi")
            .join("session-projections")
            .join(&file_name);
        fs::create_dir_all(
            duplicate_path
                .parent()
                .expect("duplicate projection should have parent"),
        )
        .expect("duplicate projection directory should create");
        fs::copy(&canonical_path, &duplicate_path).expect("matching duplicate should copy");

        let (durable, sidecars) = repository
            .load_session_projections(&[("duplicate-root".to_string(), duplicate_root.clone())])
            .expect("identical duplicate projection should deduplicate");
        assert_eq!(durable.sessions.len(), 1);
        repository
            .save_session_projection_state(&durable, &sidecars)
            .expect("deduplicated projection should checkpoint");
        assert!(canonical_path.exists());
        assert!(
            !duplicate_path.exists(),
            "非权威重复路径必须在下一次 checkpoint 删除"
        );

        let mut conflicting: SessionProjectionSnapshot = serde_json::from_str(
            &fs::read_to_string(&canonical_path).expect("canonical projection should read"),
        )
        .expect("canonical projection should parse");
        conflicting.durable.sessions[0].title = "conflicting duplicate".to_string();
        repository
            .write_json_atomically(duplicate_path.clone(), &conflicting)
            .expect("conflicting duplicate should write");
        let error = repository
            .load_session_projections(&[("duplicate-root".to_string(), duplicate_root.clone())])
            .expect_err("conflicting duplicate projection must reject recovery");
        assert!(error.to_string().contains("冲突的重复 projection"));

        let _ = fs::remove_dir_all(state_root);
        let _ = fs::remove_dir_all(duplicate_root);
    }

    #[test]
    fn moving_session_projection_updates_new_path_before_removing_old_path() {
        let state_root = unique_temp_dir("magi-session-projection-move");
        let workspace_root = unique_temp_dir("magi-session-projection-move-workspace");
        let repository = StateRepository::new(state_root.clone());
        let session_store = SessionStore::new();
        let session_id = SessionId::new("session-projection-move");
        session_store
            .create_session(session_id.clone(), "moving projection")
            .expect("session should create");
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("personal projection should save");
        let file_name = StateRepository::session_projection_file_name(&session_id);
        let old_path = repository.session_projection_root().join(&file_name);
        assert!(old_path.exists());

        let workspace_id = WorkspaceId::new("workspace-projection-move");
        let workspace_store = WorkspaceStore::new();
        workspace_store
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
            )
            .expect("workspace should register");
        repository
            .save_workspace_durable_state(&workspace_store.durable_state())
            .expect("workspace registry should save");
        session_store.bind_execution_ownership(
            session_id.clone(),
            magi_core::ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: Some(workspace_id.clone()),
                ..magi_core::ExecutionOwnership::default()
            },
        );
        session_store
            .persist_projection_with(|durable, sidecars| {
                repository.save_session_projection_state(durable, sidecars)
            })
            .expect("moved projection should save");

        let new_path = workspace_root
            .join(".magi")
            .join("session-projections")
            .join(file_name);
        assert!(new_path.exists());
        assert!(!old_path.exists(), "新路径提交成功后必须删除旧 projection");
        let (restored, restored_sidecars) = repository
            .load_session_projections(&[(workspace_id.to_string(), workspace_root.clone())])
            .expect("moved projection should restore once");
        assert_eq!(restored.sessions.len(), 1);
        assert_eq!(restored_sidecars.runtime_sidecars.len(), 1);
        assert_eq!(
            restored.sessions[0].workspace_id.as_deref(),
            Some(workspace_id.as_str())
        );

        let _ = fs::remove_dir_all(state_root);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn session_current_never_points_to_a_deleted_projection_after_checkpoint() {
        let state_root = unique_temp_dir("magi-session-current-delete-order");
        let repository = StateRepository::new(state_root.clone());
        let store = SessionStore::new();
        let retained_id = SessionId::new("session-current-retained");
        let deleted_id = SessionId::new("session-current-deleted");
        store
            .create_session(retained_id.clone(), "retained")
            .expect("retained session should create");
        store
            .create_session(deleted_id.clone(), "deleted")
            .expect("deleted session should create");
        store
            .persist_projection_with(|durable, sidecars| {
                repository.save_session_projection_state(durable, sidecars)
            })
            .expect("initial projection should save");
        store
            .delete_session(&deleted_id)
            .expect("current session should delete");
        store
            .persist_projection_with(|durable, sidecars| {
                repository.save_session_projection_state(durable, sidecars)
            })
            .expect("deletion checkpoint should save");

        let current: Option<SessionId> = serde_json::from_str(
            &fs::read_to_string(state_root.join("session-current.json"))
                .expect("current pointer should read"),
        )
        .expect("current pointer should parse");
        assert_eq!(current, Some(retained_id));
        assert!(
            !repository
                .session_projection_root()
                .join(StateRepository::session_projection_file_name(&deleted_id))
                .exists()
        );

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn session_deletion_removes_projection_and_event_log_in_one_checkpoint() {
        let state_root = unique_temp_dir("magi-session-delete-events");
        let repository = StateRepository::new(state_root.clone());
        let (store, _, _) = accepted_session_store("session-delete-events", None, 45);
        let session_id = SessionId::new("session-delete-events");
        install_test_event_authority(&repository, &store);
        store
            .persist_projection_with(|durable, sidecars| {
                repository.save_session_projection_state(durable, sidecars)
            })
            .expect("session with event log should persist");
        assert!(repository.session_event_root(&session_id).exists());

        store
            .delete_session(&session_id)
            .expect("session should delete");
        store
            .persist_projection_with(|durable, sidecars| {
                repository.save_session_projection_state(durable, sidecars)
            })
            .expect("session deletion should persist");

        assert!(
            !repository
                .session_projection_root()
                .join(StateRepository::session_projection_file_name(&session_id))
                .exists()
        );
        assert!(!repository.session_event_root(&session_id).exists());
        let (restored, _) = StateRepository::new(state_root.clone())
            .load_session_projections(&[])
            .expect("deleted session state should remain valid");
        assert!(restored.sessions.is_empty());

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn dangling_session_current_pointer_rejects_recovery() {
        let state_root = unique_temp_dir("magi-session-current-dangling");
        let repository = StateRepository::new(state_root.clone());
        repository
            .write_json_atomically(
                state_root.join("session-current.json"),
                &Some(SessionId::new("session-missing")),
            )
            .expect("dangling current pointer should write");

        let error = repository
            .load_session_projections(&[])
            .expect_err("dangling current pointer must reject recovery");
        assert!(error.to_string().contains("指向不存在的 session"));

        let _ = fs::remove_dir_all(state_root);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            UtcMillis::now().0
        ));
        fs::create_dir_all(&path).expect("temp dir should create");
        path
    }

    fn session_incident(
        notification_id: &str,
        workspace_id: &str,
        session_id: &SessionId,
        message: &str,
        created_at: UtcMillis,
    ) -> NotificationRecord {
        NotificationRecord {
            notification_id: notification_id.to_string(),
            scope: NotificationScope::Session,
            workspace_id: Some(workspace_id.to_string()),
            session_id: Some(session_id.clone()),
            kind: "incident".to_string(),
            level: Some("error".to_string()),
            title: None,
            message: message.to_string(),
            detail: None,
            error_code: None,
            failure_stage: None,
            task_id: None,
            request_id: None,
            source: Some("test".to_string()),
            created_at,
            handled: false,
            action_required: true,
            count_unread: true,
            fingerprint: notification_id.to_string(),
            occurrence_count: 1,
            resolved: false,
        }
    }

    #[test]
    fn session_projection_preserves_history_and_thread_registry() {
        let state_root = unique_temp_dir("magi-persistence-state");
        let workspace_root = unique_temp_dir("magi-persistence-workspace");
        let repository = StateRepository::new(state_root.clone());
        let session_id = SessionId::new("session-persisted-timeline");
        let now = UtcMillis::now();
        let app_incident = NotificationRecord {
            notification_id: "notification-persisted-app".to_string(),
            scope: NotificationScope::App,
            workspace_id: None,
            session_id: None,
            kind: "incident".to_string(),
            level: Some("error".to_string()),
            title: None,
            message: "恢复后的应用异常".to_string(),
            detail: None,
            error_code: None,
            failure_stage: None,
            task_id: None,
            request_id: None,
            source: Some("test".to_string()),
            created_at: now,
            handled: false,
            action_required: true,
            count_unread: true,
            fingerprint: "notification-persisted-app".to_string(),
            occurrence_count: 1,
            resolved: false,
        };
        let workspace_state = SessionDurableState {
            current_session_id: Some(session_id.clone()),
            sessions: vec![SessionRecord {
                session_id: session_id.clone(),
                title: "持久化会话".to_string(),
                status: SessionLifecycleStatus::Active,
                created_at: now,
                updated_at: now,
                message_count: Some(1),
                workspace_id: Some("workspace-persisted".to_string()),
                last_completed_at: None,
                last_viewed_at: None,
            }],
            timeline: vec![TimelineEntry {
                entry_id: "timeline-persisted-user".to_string(),
                session_id: session_id.clone(),
                kind: TimelineEntryKind::UserMessage,
                message: "恢复后的用户消息".to_string(),
                occurred_at: now,
            }],
            canonical_turns: vec![],
            notifications: vec![
                app_incident,
                session_incident(
                    "notification-persisted",
                    "workspace-persisted",
                    &session_id,
                    "恢复后的异常",
                    now,
                ),
            ],
            goals: vec![],
            plans: vec![SessionPlan {
                plan_id: PlanId::new("plan-persisted"),
                session_id: session_id.clone(),
                goal_id: None,
                revision: 1,
                language: "zh-CN".to_string(),
                state: PlanState::Active,
                items: vec![PlanItem::new(
                    PlanItemId::new("restore-plan"),
                    "恢复目标任务清单",
                    PlanItemStatus::InProgress,
                )],
                task_bindings: HashMap::new(),
                task_statuses: HashMap::new(),
                updated_at: now,
            }],
            thread_registry: vec![ExecutionThread {
                thread_id: ThreadId::new("thread-persisted"),
                session_id: session_id.clone(),
                mission_id: MissionId::new("mission-persisted"),
                role_id: "executor".to_string(),
                worker_instance_id: WorkerId::new("worker-persisted"),
                status: ExecutionThreadStatus::Active,
                created_at: now,
                last_used_at: now,
                observed_context_window_tokens: Some(32_000),
                handled_task_ids: vec![TaskId::new("task-persisted")],
                message_history: vec![ThreadChatMessage {
                    role: "tool".to_string(),
                    content: Some("exit code 1: persisted tool error".to_string()),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some("call-persisted".to_string()),
                    provider_context: Vec::new(),
                }],
            }],
            thread_context_checkpoints: vec![ThreadContextCheckpoint {
                thread_id: ThreadId::new("thread-persisted"),
                checkpoint_id: "checkpoint-persisted".to_string(),
                source_message_count: 1,
                summary_message: ThreadChatMessage {
                    role: "system".to_string(),
                    content: Some("恢复后的上下文检查点".to_string()),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                },
                reason: "context_window_pressure".to_string(),
                original_token_estimate: 32_000,
                checkpoint_token_estimate: 4_000,
                created_at: now,
                generation: 1,
                source_fingerprint: String::new(),
                model_provider: None,
                model: None,
                binding_revision: None,
                projected_request_tokens: 0,
                context_window_limit_tokens: None,
                preserved_tail_message_count: 0,
                file_fact_versions: Vec::new(),
            }],
        };
        repository
            .save_session_projection_state(
                &workspace_state,
                &SessionExecutionSidecarStoreState::default(),
            )
            .expect("workspace session state should save");

        let (merged, _) = repository
            .load_session_projections(&[(
                "workspace-persisted".to_string(),
                workspace_root.clone(),
            )])
            .expect("workspace session state should load");

        assert_eq!(merged.sessions.len(), 1);
        assert_eq!(merged.timeline.len(), 1);
        assert_eq!(merged.notifications.len(), 2);
        assert!(state_root.join("session-app-meta.json").exists());
        assert_eq!(merged.plans.len(), 1);
        assert_eq!(merged.plans[0].items[0].title, "恢复目标任务清单");
        assert_eq!(merged.thread_registry.len(), 1);
        assert_eq!(merged.thread_context_checkpoints.len(), 1);
        assert_eq!(
            merged.thread_context_checkpoints[0].checkpoint_id,
            "checkpoint-persisted"
        );
        assert_eq!(
            merged.thread_registry[0].thread_id.as_str(),
            "thread-persisted"
        );
        assert_eq!(
            merged.thread_registry[0].observed_context_window_tokens,
            Some(32_000)
        );
        assert_eq!(
            merged.thread_registry[0].message_history[0]
                .content
                .as_deref(),
            Some("exit code 1: persisted tool error")
        );
        assert_eq!(merged.current_session_id, Some(session_id));

        let _ = fs::remove_dir_all(state_root);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn canonical_event_log_overrides_stale_projection_cache() {
        let state_root = unique_temp_dir("magi-canonical-event-authority");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, turn_id, _) =
            accepted_session_store("canonical-event-authority", None, 50);
        let session_id = SessionId::new("canonical-event-authority");
        install_test_event_authority(&repository, &session_store);

        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("pending turn should persist");
        let projection_path = repository
            .session_projection_root()
            .join(StateRepository::session_projection_file_name(&session_id));
        let stale_projection = fs::read(&projection_path).expect("projection should exist");

        session_store
            .update_current_turn_status_for_turn(&session_id, Some(turn_id.as_str()), "completed")
            .expect("turn should complete");
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("terminal turn should persist");
        magi_core::fs_atomic::write_atomic(&projection_path, stale_projection)
            .expect("test should restore stale projection cache");

        let restored_repository = StateRepository::new(state_root.clone());
        let (restored, _) = restored_repository
            .load_session_projections(&[])
            .expect("canonical events should advance stale cache");
        assert_eq!(restored.canonical_turns.len(), 1);
        assert_eq!(
            restored.canonical_turns[0].status,
            magi_session_store::CanonicalTurnStatus::Completed
        );

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn canonical_projection_cannot_recreate_a_missing_event_log() {
        let state_root = unique_temp_dir("magi-canonical-event-required");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, _, _) = accepted_session_store("canonical-event-required", None, 55);
        let session_id = SessionId::new("canonical-event-required");
        install_test_event_authority(&repository, &session_store);
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("session checkpoint should persist");
        fs::remove_dir_all(repository.session_event_root(&session_id))
            .expect("event log should be removed for corruption test");

        StateRepository::new(state_root.clone())
            .load_session_projections(&[])
            .expect_err("v2 projection must not recreate missing canonical facts");

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn accepted_event_transaction_recovers_without_session_projection() {
        let state_root = unique_temp_dir("magi-accepted-event-only-recovery");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, turn_id, task_id) =
            accepted_session_store("accepted-event-only-recovery", None, 57);
        let session_id = SessionId::new("accepted-event-only-recovery");
        let acceptance = session_store
            .session_acceptance_record(&session_id, &turn_id)
            .expect("accepted session should provide a recovery record");
        let task = accepted_task(task_id.clone(), 57);

        repository
            .append_canonical_turn_transaction_with_acceptance(
                &session_id,
                &[CanonicalTurnMutation {
                    previous: None,
                    next: acceptance.canonical_turn.clone(),
                }],
                &acceptance,
                &task,
            )
            .expect("accepted event transaction should commit");
        assert!(
            !repository
                .session_projection_root()
                .join(StateRepository::session_projection_file_name(&session_id))
                .exists()
        );
        assert!(!repository.accepted_submissions_path().exists());

        let restored_repository = StateRepository::new(state_root.clone());
        let (durable, sidecars) = restored_repository
            .load_session_projections(&[])
            .expect("event-only accepted state should recover");
        assert_eq!(durable.sessions.len(), 1);
        assert_eq!(durable.timeline.len(), 1);
        assert_eq!(durable.canonical_turns.len(), 1);
        assert_eq!(durable.canonical_turns[0].turn_id, turn_id);
        assert!(sidecars.runtime_sidecar(&session_id).is_some());
        restored_repository
            .validate_session_event_log_coverage(&durable)
            .expect("recovered acceptance should own its event log");

        let recovered_acceptance = restored_repository
            .load_accepted_submissions()
            .expect("accepted event should remain available until task checkpoint");
        assert_eq!(recovered_acceptance.len(), 1);
        assert!(recovered_acceptance[0].session_checkpointed);
        assert!(!recovered_acceptance[0].task_checkpointed);
        assert_eq!(recovered_acceptance[0].task.task_id, task_id);

        restored_repository
            .save_session_projection_state(&durable, &sidecars)
            .expect("recovered session projection should checkpoint");
        let task_store = TaskStore::new();
        task_store
            .insert_task_without_checkpoint(task)
            .expect("recovered task should insert");
        restored_repository
            .checkpoint_task_store(&task_store)
            .expect("recovered task should checkpoint");
        assert!(
            restored_repository
                .load_accepted_submissions()
                .expect("accepted event should converge after task checkpoint")
                .is_empty()
        );

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn orphan_event_log_without_acceptance_is_rejected_by_coverage() {
        let state_root = unique_temp_dir("magi-canonical-event-orphan");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, _, _) = accepted_session_store("canonical-event-orphan", None, 56);
        let session_id = SessionId::new("canonical-event-orphan");
        install_test_event_authority(&repository, &session_store);
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("session checkpoint should persist");
        fs::remove_file(
            repository
                .session_projection_root()
                .join(StateRepository::session_projection_file_name(&session_id)),
        )
        .expect("projection should be removed for interrupted-write test");

        repository
            .validate_session_event_log_coverage(&session_store.durable_state())
            .expect("accepted WAL-restored session should own its event log");
        repository
            .validate_session_event_log_coverage(&SessionDurableState::default())
            .expect_err("unknown orphan event log must fail recovery");

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn legacy_layout_migrates_once_and_archives_sources() {
        let state_root = unique_temp_dir("magi-state-layout-migration");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, _, task_id) =
            accepted_session_store("legacy-layout-migration", None, 60);
        let task_store = TaskStore::new();
        task_store
            .insert_task_without_checkpoint(accepted_task(task_id, 60))
            .expect("accepted task should insert");
        let mut legacy_sidecars = session_store.execution_sidecar_store_state();
        let mut orphan_sidecar = legacy_sidecars
            .runtime_sidecars
            .first()
            .cloned()
            .expect("accepted session should have a sidecar");
        orphan_sidecar.session_id = SessionId::new("legacy-layout-orphan-sidecar");
        orphan_sidecar.ownership.session_id = None;
        if let Some(chain) = orphan_sidecar.active_execution_chain.as_mut() {
            chain.session_id = orphan_sidecar.session_id.clone();
        }
        legacy_sidecars.upsert_runtime_sidecar(orphan_sidecar);

        repository
            .write_json_atomically(
                state_root.join("sessions.json"),
                &session_store.durable_state(),
            )
            .expect("legacy sessions should persist");
        repository
            .write_json_atomically(state_root.join("session-sidecars.json"), &legacy_sidecars)
            .expect("legacy sidecars should persist");
        repository
            .write_json_atomically(state_root.join("task-store.json"), &task_store.checkpoint())
            .expect("legacy task checkpoint should persist");

        repository
            .migrate_legacy_state_layout(&[])
            .expect("legacy layout should migrate");
        assert!(state_root.join("state-layout.json").exists());
        assert!(!state_root.join("sessions.json").exists());
        assert!(!state_root.join("session-sidecars.json").exists());
        assert!(!state_root.join("task-store.json").exists());
        assert!(
            state_root
                .join("migrations/legacy-v1/archive/sessions.json")
                .exists()
        );
        assert!(
            repository
                .session_event_root(&SessionId::new("legacy-layout-migration"))
                .exists()
        );
        let (restored, _) = StateRepository::new(state_root.clone())
            .load_session_projections(&[])
            .expect("migrated sessions should restore");
        assert_eq!(restored.sessions.len(), 1);
        let (_, restored_sidecars) = StateRepository::new(state_root.clone())
            .load_session_projections(&[])
            .expect("migrated sidecars should restore");
        assert!(restored_sidecars
            .runtime_sidecars
            .iter()
            .all(|sidecar| sidecar.session_id != SessionId::new("legacy-layout-orphan-sidecar")));
        assert!(
            TaskStore::restore_from_projection_directory(&repository.task_store_projection_path())
                .expect("migrated task projections should read")
                .is_some()
        );

        repository
            .migrate_legacy_state_layout(&[])
            .expect("committed migration should be idempotent");
        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn legacy_layout_reconstructs_superseded_turn_event_path() {
        let state_root = unique_temp_dir("magi-state-layout-superseded-turn");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, _, _) = accepted_session_store("legacy-superseded-turn", None, 65);
        let mut durable = session_store.durable_state();
        let turn = durable
            .canonical_turns
            .first_mut()
            .expect("accepted session should contain canonical turn");
        turn.status = magi_session_store::CanonicalTurnStatus::Superseded;
        turn.completed_at = Some(UtcMillis(66));

        repository
            .write_json_atomically(state_root.join("sessions.json"), &durable)
            .expect("legacy sessions should persist");
        let loaded = repository
            .load_legacy_session_state(&repository.legacy_state_paths(&[]))
            .expect("legacy session should load");
        assert_eq!(
            loaded.canonical_turns[0].status,
            magi_session_store::CanonicalTurnStatus::Superseded
        );
        let converted = SessionStore::convert_v1_persisted_parts(
            loaded.clone(),
            SessionExecutionSidecarStoreState::default(),
        )
        .expect("legacy session should convert");
        assert_eq!(
            converted
                .durable_state()
                .canonical_turns
                .iter()
                .find(|turn| turn.turn_id == "turn-legacy-superseded-turn-65")
                .expect("original converted turn should remain")
                .status,
            magi_session_store::CanonicalTurnStatus::Superseded
        );
        repository
            .migrate_legacy_state_layout(&[])
            .expect("superseded turn should migrate through Cancelled");

        let event_projection = SessionConversationProjection::load(
            &repository.session_event_root(&SessionId::new("legacy-superseded-turn")),
            &SessionId::new("legacy-superseded-turn"),
        )
        .expect("migrated event projection should load");
        assert_eq!(
            event_projection
                .canonical_turns()
                .iter()
                .find(|turn| turn.turn_id == "turn-legacy-superseded-turn-65")
                .expect("original superseded turn should remain")
                .status,
            magi_session_store::CanonicalTurnStatus::Superseded
        );
        let (restored, _) = StateRepository::new(state_root.clone())
            .load_session_projections(&[])
            .expect("migrated superseded turn should restore");
        assert_eq!(
            restored
                .canonical_turns
                .iter()
                .find(|turn| turn.turn_id == "turn-legacy-superseded-turn-65")
                .expect("original superseded turn should restore")
                .status,
            magi_session_store::CanonicalTurnStatus::Superseded
        );
        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn workspace_legacy_layout_migrates_to_its_registered_root() {
        let state_root = unique_temp_dir("magi-workspace-layout-migration");
        let workspace_root = unique_temp_dir("magi-workspace-layout-migration-root");
        let repository = StateRepository::new(state_root.clone());
        let workspace_id = WorkspaceId::new("workspace-layout-migration");
        let workspace_store = WorkspaceStore::new();
        workspace_store
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
            )
            .expect("workspace should register");
        repository
            .save_workspace_durable_state(&workspace_store.durable_state())
            .expect("workspace registry should persist");
        let (session_store, _, _) =
            accepted_session_store("workspace-layout-session", Some(workspace_id.as_str()), 61);
        repository
            .write_json_atomically(
                workspace_root.join(".magi/sessions.json"),
                &session_store.durable_state(),
            )
            .expect("workspace legacy sessions should persist");
        repository
            .write_json_atomically(
                state_root.join("session-sidecars.json"),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("legacy sidecars should persist");
        let workspace_roots = vec![(workspace_id.to_string(), workspace_root.clone())];

        repository
            .migrate_legacy_state_layout(&workspace_roots)
            .expect("workspace legacy layout should migrate");

        assert!(!workspace_root.join(".magi/sessions.json").exists());
        assert!(
            workspace_root
                .join(".magi/session-projections/workspace-layout-session.json")
                .exists()
        );
        assert!(
            state_root
                .join("migrations/legacy-v1/archive/workspaces/workspace-layout-migration/sessions.json")
                .exists()
        );
        let (restored, _) = StateRepository::new(state_root.clone())
            .load_session_projections(&workspace_roots)
            .expect("workspace migrated session should restore");
        assert_eq!(restored.sessions.len(), 1);
        assert_eq!(
            restored.sessions[0].workspace_id.as_deref(),
            Some(workspace_id.as_str())
        );

        let _ = fs::remove_dir_all(state_root);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn interrupted_layout_migration_restarts_from_legacy_authority() {
        let state_root = unique_temp_dir("magi-layout-migration-resume");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, _, _) = accepted_session_store("layout-migration-resume", None, 62);
        repository
            .write_json_atomically(
                state_root.join("sessions.json"),
                &session_store.durable_state(),
            )
            .expect("legacy sessions should persist");
        repository
            .write_json_atomically(
                state_root.join("state-layout-migration.json"),
                &StateLayoutMigration {
                    source_version: 1,
                    target_version: STATE_LAYOUT_VERSION,
                },
            )
            .expect("migration marker should persist");
        fs::create_dir_all(repository.session_projection_root())
            .expect("partial projection root should create");
        fs::write(
            repository.session_projection_root().join("partial.json"),
            b"partial",
        )
        .expect("partial migration output should write");

        repository
            .migrate_legacy_state_layout(&[])
            .expect("interrupted migration should restart from legacy source");

        assert!(state_root.join("state-layout.json").exists());
        assert!(!state_root.join("state-layout-migration.json").exists());
        assert!(
            !repository
                .session_projection_root()
                .join("partial.json")
                .exists()
        );
        let (restored, _) = StateRepository::new(state_root.clone())
            .load_session_projections(&[])
            .expect("resumed migration should restore");
        assert_eq!(restored.sessions.len(), 1);

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn committed_v2_layout_rejects_reintroduced_legacy_files() {
        let state_root = unique_temp_dir("magi-layout-v2-rejects-legacy");
        let repository = StateRepository::new(state_root.clone());
        repository
            .write_json_atomically(
                state_root.join("state-layout.json"),
                &StateLayoutMarker {
                    version: STATE_LAYOUT_VERSION,
                },
            )
            .expect("v2 marker should persist");
        fs::write(state_root.join("sessions.json"), b"{}")
            .expect("unexpected legacy file should write");

        repository
            .migrate_legacy_state_layout(&[])
            .expect_err("committed v2 must not read or remove reintroduced legacy state");
        assert!(state_root.join("sessions.json").exists());

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn mixed_legacy_and_unmarked_new_layout_is_merged_without_data_loss() {
        let state_root = unique_temp_dir("magi-state-layout-conflict");
        let repository = StateRepository::new(state_root.clone());
        let (legacy_store, _, _) = accepted_session_store("legacy-layout-session", None, 63);
        let (new_store, _, _) = accepted_session_store("unmarked-layout-session", None, 64);
        repository
            .write_json_atomically(
                state_root.join("sessions.json"),
                &legacy_store.durable_state(),
            )
            .expect("legacy session file should persist");
        fs::create_dir_all(repository.session_projection_root())
            .expect("new projection directory should exist");
        let new_session_id = SessionId::new("unmarked-layout-session");
        repository
            .write_json_atomically(
                repository.session_projection_root().join(
                    StateRepository::session_projection_file_name(&new_session_id),
                ),
                &serde_json::json!({
                    "durable": new_store
                        .durable_state()
                        .durable_state_for_session(&new_session_id),
                    "sidecar": null,
                }),
            )
            .expect("unmarked session projection should persist");

        repository
            .migrate_legacy_state_layout(&[])
            .expect("mixed layouts should be merged into one committed layout");
        let (restored, _) = StateRepository::new(state_root.clone())
            .load_session_projections(&[])
            .expect("merged layout should restore");
        assert_eq!(restored.sessions.len(), 2);
        assert!(!state_root.join("sessions.json").exists());
        assert!(state_root.join("state-layout.json").exists());

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn corrupt_legacy_session_state_stops_migration() {
        let state_root = unique_temp_dir("magi-state-layout-corrupt");
        let repository = StateRepository::new(state_root.clone());
        fs::write(state_root.join("sessions.json"), b"{not-json")
            .expect("corrupt legacy file should exist");

        repository
            .migrate_legacy_state_layout(&[])
            .expect_err("corrupt legacy state must not become an empty migration");
        assert!(state_root.join("sessions.json").exists());
        assert!(!state_root.join("state-layout.json").exists());

        let _ = fs::remove_dir_all(state_root);
    }
}
