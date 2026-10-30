mod project_migration;

use super::config::DaemonError;
use super::session_event_log::SessionConversationProjection;
use magi_core::{DomainError, DomainResult, SessionId, Task};
use magi_event_bus::AuditUsageLedgerSnapshot;
use magi_knowledge_store::KnowledgeState;
use magi_orchestrator::task_store::{TaskStore, TaskStoreSnapshot};
use magi_session_store::{
    CanonicalTurnMutation, SessionAcceptanceRecord, SessionDurableState, SessionEventWriter,
    SessionExecutionSidecarStoreState, SessionInputMutation, SessionRuntimeSidecar, SessionStore,
    UnavailableSession,
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

const SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub(crate) struct StateRepository {
    state_root: PathBuf,
    write_lock: Arc<Mutex<()>>,
    session_projection_cache: Arc<Mutex<SessionProjectionCache>>,
    session_event_cache: Arc<Mutex<HashMap<magi_core::SessionId, SessionConversationProjection>>>,
    event_accepted_submissions: Arc<Mutex<Vec<AcceptedSubmissionRecord>>>,
    unavailable_sessions: Arc<Mutex<Vec<UnavailableSession>>>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AcceptedSubmissionRecord {
    pub session: SessionAcceptanceRecord,
    pub task: Option<Task>,
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

/// 已落盘文件内容的摘要。
///
/// 缓存只需要回答“这次要写的内容和上次写下的是否一样”，不需要保留全文。全文常驻会让
/// 内存随全部会话历史的总量线性增长（本仓库实测仅这一项就有 175 MB），完整保存时还要
/// 整份深拷贝。长度加 64 位哈希足以判定相同内容；误判为相同的概率可忽略，且只会导致跳过
/// 一次本可幂等重写的写入。
#[derive(Clone, Debug, PartialEq, Eq)]
struct ContentDigest {
    len: usize,
    hash: u64,
}

impl ContentDigest {
    fn of(content: &str) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        Self {
            len: content.len(),
            hash: hasher.finish(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct SessionProjectionCache {
    snapshots: HashMap<magi_core::SessionId, (PathBuf, ContentDigest)>,
    pending_removals: HashSet<PathBuf>,
    pending_event_removals: HashSet<PathBuf>,
    global: Option<(PathBuf, ContentDigest)>,
    app_meta: Option<(PathBuf, ContentDigest)>,
    workspace_meta: HashMap<String, (PathBuf, ContentDigest)>,
}

/// 事件回放缓存里同时保持回合驻留的会话数上限。
///
/// 回合事实在 SessionStore 里已有一份；缓存只需要让最近写入过的少数会话保持驻留以便
/// 增量追加事件，其余会话只留事件游标与 accepted 记录。
const MAX_RESIDENT_EVENT_PROJECTIONS: usize = 6;

fn trim_resident_event_projections(
    cache: &mut HashMap<magi_core::SessionId, SessionConversationProjection>,
) {
    let mut resident = cache
        .iter()
        .filter(|(_, projection)| projection.is_resident())
        .map(|(session_id, projection)| (session_id.clone(), projection.last_touched()))
        .collect::<Vec<_>>();
    if resident.len() <= MAX_RESIDENT_EVENT_PROJECTIONS {
        return;
    }
    resident.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (session_id, _) in resident.into_iter().skip(MAX_RESIDENT_EVENT_PROJECTIONS) {
        if let Some(projection) = cache.get_mut(&session_id) {
            projection.release_canonical_turns();
        }
    }
}

const STATE_LAYOUT_VERSION: u32 = 19;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StateLayoutMarker {
    version: u32,
}

impl StateRepository {
    pub(crate) fn new(state_root: PathBuf) -> Self {
        Self {
            state_root,
            write_lock: Arc::new(Mutex::new(())),
            session_projection_cache: Arc::new(Mutex::new(SessionProjectionCache::default())),
            session_event_cache: Arc::new(Mutex::new(HashMap::new())),
            event_accepted_submissions: Arc::new(Mutex::new(Vec::new())),
            unavailable_sessions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn shared_write_lock(&self) -> Arc<Mutex<()>> {
        self.write_lock.clone()
    }

    pub(crate) fn save_session_projection_state(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
    ) -> Result<(), DaemonError> {
        let workspace_roots = self.workspace_projection_roots()?;
        self.save_session_projection_parts(durable, sidecars, &workspace_roots, None, false)
    }

    /// 只提交指定 session 的 durable projection。
    ///
    /// canonical event 已经按 session 独立落盘；sidecar flush 只需更新发生过
    /// 运行态变更的 session 文件，并维护全局 current/meta 文件。完整 snapshot
    /// 仍由 `save_session_projection_state` 保留给关机和显式一致性操作。
    pub(crate) fn save_session_projection_state_for_sessions(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
        session_ids: &[SessionId],
    ) -> Result<(), DaemonError> {
        let workspace_roots = self.workspace_projection_roots()?;
        let changed = session_ids.iter().cloned().collect::<HashSet<_>>();
        self.save_session_projection_parts(
            durable,
            sidecars,
            &workspace_roots,
            Some(&changed),
            false,
        )
    }

    pub(crate) fn save_session_projection_partial_state_with_roots(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
        session_ids: &[SessionId],
        workspace_roots: &HashMap<String, PathBuf>,
    ) -> Result<(), DaemonError> {
        let changed = session_ids.iter().cloned().collect::<HashSet<_>>();
        self.save_session_projection_parts(durable, sidecars, workspace_roots, Some(&changed), true)
    }

    /// 持久化导航状态时只提交当前指针，以及缺失的目标会话 projection。
    ///
    /// 导航不改变任何会话事实，因此不能为了保存一个指针重新遍历、重放并序列化
    /// 全部历史会话。新建 materialized session 没有 projection 时才在这里补写该
    /// 单个会话；已有 projection 则保持原文件不动。
    pub(crate) fn save_session_navigation_state(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
        target_session_id: Option<&SessionId>,
    ) -> Result<(), DaemonError> {
        let workspace_roots = self.workspace_projection_roots()?;
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
        // 导航只提交一个当前指针（以及在目标会话 projection 缺失时补一份）。缓存里保存着全部会话的
        // projection 全文与对话事件，绝不能为此整体克隆一份再替换：那会让每次切换会话的代价与全部
        // 会话历史的总量成正比（本仓库实测 300 MB 级，单次导航 1–2 秒纯用于克隆与释放）。
        // 这里只记录很小的待应用变更，等事务提交成功后再就地写入缓存；失败时缓存原样不动。
        let mut pending_snapshot: Option<(SessionId, (PathBuf, ContentDigest))> = None;
        let mut rebuilt_event_cache = None;
        let mut writes = Vec::new();

        match (durable.current_session_id.as_ref(), target_session_id) {
            (Some(current), Some(target)) if current != target => {
                return Err(DaemonError::internal(format!(
                    "导航目标与 current session 不一致: {target} != {current}"
                )));
            }
            (Some(current), None) => {
                return Err(DaemonError::internal(format!(
                    "草稿导航仍保留 current session 指针: {current}"
                )));
            }
            _ => {}
        }

        if let Some(session_id) = target_session_id {
            let session = durable
                .sessions
                .iter()
                .find(|session| &session.session_id == session_id)
                .ok_or_else(|| {
                    DaemonError::internal(format!(
                        "导航目标 session 不存在，拒绝写入 current 指针: {session_id}"
                    ))
                })?;
            let path = self.session_projection_path(
                session_id,
                session.workspace_id.as_deref(),
                &workspace_roots,
            )?;
            if !path.exists() {
                // 只有需要补建缺失的 projection 时才切出目标会话的完整状态。
                let target = durable.durable_state_for_session(session_id);
                let sidecar = sidecars
                    .runtime_sidecars
                    .iter()
                    .find(|sidecar| sidecar.session_id == *session_id)
                    .cloned();
                // 极少发生：补建缺失的 projection 需要构造对话事件，只有这时才克隆事件缓存。
                let mut next_event_cache = event_cache.clone();
                let (path, content) = self.build_session_projection_content(
                    &target,
                    sidecar,
                    &workspace_roots,
                    session_id,
                    &mut next_event_cache,
                    false,
                )?;
                writes.push(SessionProjectionWrite {
                    path: path.clone(),
                    content: content.clone(),
                });
                pending_snapshot = Some((session_id.clone(), (path, ContentDigest::of(&content))));
                rebuilt_event_cache = Some(next_event_cache);
            }
        }

        let current_path = self.state_root.join("session-current.json");
        let current_content = serde_json::to_vec_pretty(&durable.current_session_id)
            .map_err(DaemonError::from)
            .and_then(|content| {
                String::from_utf8(content).map_err(|error| {
                    DaemonError::internal(format!("session current state 不是 UTF-8: {error}"))
                })
            })?;
        let current_digest = ContentDigest::of(&current_content);
        if cache
            .global
            .as_ref()
            .map(|(_, previous)| previous != &current_digest)
            .unwrap_or(true)
        {
            writes.push(SessionProjectionWrite {
                path: current_path.clone(),
                content: current_content,
            });
        }
        let next_global = (current_path, current_digest);

        let transaction = SessionProjectionTransaction {
            schema_version: SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION,
            transaction_id: format!("session-navigation-{}", magi_core::UtcMillis::now().0),
            writes,
            removals: Vec::new(),
        };
        self.commit_session_projection_transaction_locked(&transaction, &workspace_roots)?;
        self.ensure_state_layout_marker_locked()?;
        // 事务已提交：就地应用两处小变更，不替换整份缓存。
        if let Some((session_id, snapshot)) = pending_snapshot {
            cache.snapshots.insert(session_id, snapshot);
        }
        cache.global = Some(next_global);
        if let Some(next_event_cache) = rebuilt_event_cache {
            *event_cache = next_event_cache;
        }
        Ok(())
    }

    /// 把 projection 中的 canonical turn 快照转换为事件日志的初始写入序列。
    fn initial_canonical_mutations(
        mut turns: Vec<magi_session_store::CanonicalTurn>,
    ) -> Vec<CanonicalTurnMutation> {
        turns.sort_by(|left, right| {
            left.turn_seq
                .cmp(&right.turn_seq)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        let mut mutations = Vec::with_capacity(turns.len());
        for next in turns {
            if next.status == magi_session_store::CanonicalTurnStatus::Superseded {
                // 最终快照没有记录 Superseded 必须经过 Cancelled 的中间事件；
                // 初始化 canonical event 时补齐这段状态机路径。
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
        mutations
    }

    fn import_workspace_projection_events(
        &self,
        snapshot: &mut SessionProjectionSnapshot,
        projection_path: &Path,
        original_projection_content: &str,
        workspace_roots: &HashMap<String, PathBuf>,
    ) -> Result<(SessionConversationProjection, String), DaemonError> {
        let session = snapshot
            .durable
            .sessions
            .first()
            .expect("validated session projection must contain one session");
        let session_id = session.session_id.clone();
        let mutations = Self::initial_canonical_mutations(
            snapshot
                .durable
                .canonical_turns
                .iter()
                .filter(|turn| turn.session_id == session_id)
                .cloned()
                .collect(),
        );
        if mutations.is_empty() {
            return Err(DaemonError::internal(format!(
                "workspace projection 缺少可导入的 canonical turn: {}",
                projection_path.display()
            )));
        }

        let event_root = self.session_event_root(&session_id);
        if event_root.exists() {
            return Err(DaemonError::internal(format!(
                "workspace projection 导入要求 canonical event 根不存在: {}",
                event_root.display()
            )));
        }
        let empty_projection = SessionConversationProjection::load(&event_root, &session_id)?;
        let prepared = empty_projection.prepare_transaction_write(
            &event_root,
            &session_id,
            &mutations,
            &snapshot
                .durable
                .inputs
                .iter()
                .cloned()
                .map(|next| SessionInputMutation {
                    previous: None,
                    next,
                })
                .collect::<Vec<_>>(),
        )?;
        let original_event_seq = snapshot.canonical_event_seq;
        snapshot.canonical_event_seq = prepared.projection.last_event_seq();
        Self::validate_cached_canonical_projection(
            &snapshot.durable.canonical_turns,
            snapshot.canonical_event_seq,
            &prepared.projection,
            projection_path,
        )?;
        let projection_content = if snapshot.canonical_event_seq == original_event_seq {
            original_projection_content.to_string()
        } else {
            serde_json::to_vec_pretty(snapshot)
                .map_err(DaemonError::from)
                .and_then(|content| {
                    String::from_utf8(content).map_err(|error| {
                        DaemonError::internal(format!(
                            "workspace session projection 不是 UTF-8: {error}"
                        ))
                    })
                })?
        };
        let mut writes = vec![SessionProjectionWrite {
            path: prepared.path,
            content: prepared.content,
        }];
        if snapshot.canonical_event_seq != original_event_seq {
            writes.push(SessionProjectionWrite {
                path: projection_path.to_path_buf(),
                content: projection_content.clone(),
            });
        }
        let transaction = SessionProjectionTransaction {
            schema_version: SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION,
            transaction_id: format!(
                "workspace-session-import-{}-{}",
                magi_core::UtcMillis::now().0,
                session_id
            ),
            writes,
            removals: Vec::new(),
        };
        self.commit_session_projection_transaction_locked(&transaction, workspace_roots)?;
        Ok((prepared.projection, projection_content))
    }

    pub(crate) fn load_session_projections(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<(SessionDurableState, SessionExecutionSidecarStoreState), DaemonError> {
        self.load_session_projections_scoped(workspace_roots, false)
    }

    pub(crate) fn load_project_session_projections(
        &self,
        project: &magi_workspace::WorkspaceRecord,
    ) -> Result<
        (
            SessionDurableState,
            SessionExecutionSidecarStoreState,
            Vec<UnavailableSession>,
        ),
        DaemonError,
    > {
        // 热导入使用独立读取缓存和同一持久写锁。SessionStore 校验冲突失败时，
        // 不能先覆盖其它会话的缓存；后续正常写入会从同一事件日志按需加载。
        let mut reader = Self::new(self.state_root.clone());
        reader.write_lock = self.write_lock.clone();
        let (durable, sidecars) = reader.load_session_projections_scoped(
            &[(project.workspace_id.to_string(), project.native_root_path())],
            true,
        )?;
        Ok((durable, sidecars, reader.unavailable_sessions()))
    }

    fn load_session_projections_scoped(
        &self,
        workspace_roots: &[(String, PathBuf)],
        project_only: bool,
    ) -> Result<(SessionDurableState, SessionExecutionSidecarStoreState), DaemonError> {
        self.recover_session_projection_transaction(workspace_roots)?;
        let _project_read_guard = project_only.then(|| {
            self.write_lock
                .lock()
                .expect("state repository write lock poisoned")
        });
        self.restore_quarantined_session_events(workspace_roots)?;
        let mut durable = SessionDurableState::default();
        let mut sidecars = SessionExecutionSidecarStoreState::default();
        let mut cache = SessionProjectionCache::default();
        let mut event_cache = HashMap::new();
        let mut event_accepted_submissions = Vec::new();
        let mut loaded_snapshots = HashSet::<magi_core::SessionId>::new();
        let mut unavailable = Vec::<UnavailableSession>::new();
        let workspace_root_by_id = workspace_roots.iter().cloned().collect::<HashMap<_, _>>();
        let mut roots = if project_only {
            Vec::new()
        } else {
            vec![(String::new(), self.state_root.clone())]
        };
        roots.extend(workspace_roots.iter().cloned());

        for (workspace_id, _workspace_root) in &roots {
            let projection_root = if workspace_id.is_empty() {
                self.session_projection_root()
            } else {
                self.project_storage_root(workspace_id)
                    .join(".magi")
                    .join("session-projections")
            };
            if projection_root.exists() {
                for entry in fs::read_dir(&projection_root)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.extension().and_then(|value| value.to_str()) != Some("json") {
                        continue;
                    }
                    // 单个会话的投影或事件日志损坏只让该会话不可用：原文件保留在原处，
                    // 记录原因供界面提示，其余会话照常启动。
                    let loaded = (|| -> Result<_, DaemonError> {
                        let mut content = fs::read_to_string(&path)?;
                        let mut snapshot: SessionProjectionSnapshot =
                            serde_json::from_str(&content).map_err(|error| {
                                DaemonError::internal(format!(
                                    "解析 session projection 失败 {}: {error}",
                                    path.display()
                                ))
                            })?;
                        let session_id = Self::validate_session_projection(&snapshot, &path)?;
                        let session = snapshot
                            .durable
                            .sessions
                            .first()
                            .expect("projection validation requires one session");
                        match (workspace_id.is_empty(), session.workspace_id.as_deref()) {
                            (true, Some(actual_workspace_id)) => {
                                return Err(DaemonError::internal(format!(
                                    "全局 session projection 包含 workspace 归属，拒绝自动移动: {} ({actual_workspace_id})",
                                    path.display()
                                )));
                            }
                            (false, Some(actual_workspace_id))
                                if actual_workspace_id == workspace_id => {}
                            (false, actual_workspace_id) => {
                                return Err(DaemonError::internal(format!(
                                    "workspace session projection 归属与扫描根不一致: expected={workspace_id}, actual={} ({})",
                                    actual_workspace_id.unwrap_or("<none>"),
                                    path.display()
                                )));
                            }
                            (true, None) => {}
                        }
                        let expected_path = self.session_projection_path(
                            &session_id,
                            session.workspace_id.as_deref(),
                            &workspace_root_by_id,
                        )?;
                        if path != expected_path {
                            return Err(DaemonError::internal(format!(
                                "session projection 不在规范归属路径，拒绝自动移动: {} != {}",
                                path.display(),
                                expected_path.display()
                            )));
                        }
                        if loaded_snapshots.contains(&session_id) {
                            let previous_path = cache
                                .snapshots
                                .get(&session_id)
                                .map(|(cached_path, _)| cached_path.display().to_string())
                                .unwrap_or_else(|| "<unknown>".to_string());
                            return Err(DaemonError::internal(format!(
                                "session {} 存在重复 projection，拒绝自动移动或删除: {} 与 {}，规范路径为 {}",
                                session_id,
                                previous_path,
                                path.display(),
                                expected_path.display()
                            )));
                        }
                        let event_root = self.session_event_root(&session_id);
                        let event_root_existed = event_root.exists();
                        let mut event_projection =
                            SessionConversationProjection::load(&event_root, &session_id)?;
                        if snapshot.canonical_event_seq > event_projection.last_event_seq() {
                            if !workspace_id.is_empty()
                                && !event_root_existed
                                && event_projection.last_event_seq() == 0
                                && !snapshot.durable.canonical_turns.is_empty()
                            {
                                let original_projection_content = content.clone();
                                (event_projection, content) = self
                                    .import_workspace_projection_events(
                                        &mut snapshot,
                                        &path,
                                        &original_projection_content,
                                        &workspace_root_by_id,
                                    )?;
                                warn!(
                                    %session_id,
                                    projection = %path.display(),
                                    canonical_event_seq = snapshot.canonical_event_seq,
                                    "已将可移植工作区 session projection 导入当前 canonical event 状态根"
                                );
                            } else {
                                return Err(DaemonError::internal(format!(
                                    "session projection 的 canonical event 游标超前: {} > {} ({})",
                                    snapshot.canonical_event_seq,
                                    event_projection.last_event_seq(),
                                    path.display()
                                )));
                            }
                        }
                        Self::validate_cached_canonical_projection(
                            &snapshot.durable.canonical_turns,
                            snapshot.canonical_event_seq,
                            &event_projection,
                            &path,
                        )?;
                        snapshot.durable.canonical_turns =
                            event_projection.canonical_turns().to_vec();
                        snapshot.durable.inputs = event_projection.inputs().to_vec();
                        // canonical event 是唯一事实源；sidecar 只是执行恢复缓存。
                        // event 游标相等只说明 durable canonical 已同步，不能证明 sidecar
                        // 没有停留在较早的活动快照，因此每次恢复都从事件结果重建它。
                        if let Some(sidecar) = snapshot.sidecar.as_mut() {
                            SessionStore::rebuild_sidecar_projection_from_canonical(
                                sidecar,
                                event_projection.canonical_turns(),
                            )
                            .map_err(|error| {
                                DaemonError::internal(format!(
                                    "canonical event 无法重建 sidecar projection {}: {error}",
                                    path.display()
                                ))
                            })?;
                        }
                        Ok((session_id, snapshot, content, event_projection))
                    })();
                    let (session_id, snapshot, content, event_projection) = match loaded {
                        Ok(loaded) => loaded,
                        Err(error) => {
                            unavailable.push(Self::unavailable_session_from_path(
                                &path,
                                (!workspace_id.is_empty()).then(|| workspace_id.clone()),
                                &error,
                            ));
                            continue;
                        }
                    };
                    if let Some(sidecar) = snapshot.sidecar {
                        sidecars.upsert_runtime_sidecar(sidecar);
                    }
                    durable.append_state_without_current(snapshot.durable);
                    for record in event_projection.accepted_submissions() {
                        Self::merge_event_acceptance_into_projection(
                            &mut durable,
                            &mut sidecars,
                            record,
                        );
                    }
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
                    loaded_snapshots.insert(session_id.clone());
                    event_cache.insert(session_id.clone(), event_projection);
                    cache
                        .snapshots
                        .insert(session_id, (path, ContentDigest::of(&content)));
                }
            }

            if !workspace_id.is_empty() {
                let meta_path = self
                    .project_storage_root(workspace_id)
                    .join(".magi")
                    .join("session-workspace-meta.json");
                if meta_path.exists() {
                    let content = fs::read_to_string(&meta_path)?;
                    let meta: SessionDurableState = serde_json::from_str(&content)?;
                    durable.notifications.extend(meta.notifications);
                    let digest = ContentDigest::of(&content);
                    cache
                        .workspace_meta
                        .insert(workspace_id.clone(), (meta_path, digest));
                }
            }
        }

        // accepted event 可能已经 durable，但对应的 session projection 尚未写入。
        // 事件目录本身是权威源，必须从目录发现并恢复这类 session，不能把它误判成
        // 没有归属的孤立目录后丢弃。
        let event_parent = self.state_root.join("session-events");
        if !project_only && event_parent.exists() {
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
                // 投影已判定不可用的会话保留原事件目录，不再按 event-only 会话恢复。
                if unavailable
                    .iter()
                    .any(|session| self.session_event_root(&session.session_id) == event_root)
                {
                    continue;
                }
                let loaded = (|| -> Result<_, DaemonError> {
                    let session_id =
                        SessionConversationProjection::read_session_id_from_root(&event_root)?;
                    if self.session_event_root(&session_id) != event_root {
                        return Err(DaemonError::internal(format!(
                            "canonical event 目录与 session 归属不一致: {}",
                            event_root.display()
                        )));
                    }
                    if event_cache.contains_key(&session_id) {
                        return Ok(None);
                    }
                    let event_projection =
                        SessionConversationProjection::load(&event_root, &session_id)?;
                    if event_projection.accepted_submissions().is_empty() {
                        return Ok(Some((session_id, event_projection, None)));
                    }
                    // 先只读校验并在空容器里构建 sidecar，确认整个会话可恢复后才提交到
                    // 启动状态，避免损坏会话留下半合并的事实。
                    let mut staged_durable = SessionDurableState::default();
                    let mut staged_sidecars = SessionExecutionSidecarStoreState::default();
                    let mut accepted = Vec::new();
                    for record in event_projection.accepted_submissions() {
                        let mut record = record.clone();
                        record.session_checkpointed = true;
                        Self::merge_event_acceptance_into_projection(
                            &mut staged_durable,
                            &mut staged_sidecars,
                            &record,
                        );
                        accepted.push(record);
                    }
                    let mut new_turns = Vec::new();
                    for turn in event_projection.canonical_turns() {
                        match durable
                            .canonical_turns
                            .iter()
                            .find(|existing| existing.turn_id == turn.turn_id)
                        {
                            Some(existing) if existing != turn => {
                                return Err(DaemonError::internal(format!(
                                    "canonical event 与 session projection 的 turn 冲突: {}",
                                    turn.turn_id
                                )));
                            }
                            Some(_) => {}
                            None => new_turns.push(turn.clone()),
                        }
                    }
                    let mut sidecar = staged_sidecars.runtime_sidecar(&session_id).ok_or_else(
                        || {
                            DaemonError::internal(format!(
                                "accepted canonical event 缺少 event-only sidecar projection: {}",
                                event_root.display()
                            ))
                        },
                    )?;
                    SessionStore::rebuild_sidecar_projection_from_canonical(
                        &mut sidecar,
                        event_projection.canonical_turns(),
                    )
                    .map_err(|error| {
                        DaemonError::internal(format!(
                            "canonical event 无法重建 event-only sidecar projection {}: {error}",
                            event_root.display()
                        ))
                    })?;
                    Ok(Some((
                        session_id,
                        event_projection,
                        Some((accepted, new_turns, sidecar)),
                    )))
                })();
                match loaded {
                    Ok(None) => {}
                    Ok(Some((session_id, event_projection, staged))) => {
                        // 没有 accepted 事实时无法从事件本身重建 session 元数据；最终由
                        // coverage 校验报告该目录确实是损坏的孤立事件日志。
                        if let Some((accepted, new_turns, sidecar)) = staged {
                            for record in &accepted {
                                Self::merge_event_acceptance_into_projection(
                                    &mut durable,
                                    &mut sidecars,
                                    record,
                                );
                            }
                            durable.canonical_turns.extend(new_turns);
                            sidecars.upsert_runtime_sidecar(sidecar);
                            event_accepted_submissions.extend(accepted);
                        }
                        durable
                            .inputs
                            .retain(|input| input.session_id != session_id);
                        durable
                            .inputs
                            .extend(event_projection.inputs().iter().cloned());
                        event_cache.insert(session_id, event_projection);
                    }
                    Err(error) => unavailable.push(Self::unavailable_session_from_path(
                        &event_root,
                        None,
                        &error,
                    )),
                }
            }
        }

        let current_path = self.state_root.join("session-current.json");
        if !project_only && current_path.exists() {
            let content = fs::read_to_string(&current_path)?;
            let current: Option<magi_core::SessionId> =
                serde_json::from_str(&content).map_err(|error| {
                    DaemonError::internal(format!(
                        "解析 session current state 失败 {}: {error}",
                        current_path.display()
                    ))
                })?;
            durable.current_session_id = current;
            cache.global = Some((current_path, ContentDigest::of(&content)));
        }

        let app_meta_path = self.state_root.join("session-app-meta.json");
        if !project_only && app_meta_path.exists() {
            let content = fs::read_to_string(&app_meta_path)?;
            let meta: SessionDurableState = serde_json::from_str(&content)?;
            durable.notifications.extend(meta.notifications);
            cache.app_meta = Some((app_meta_path, ContentDigest::of(&content)));
        }

        // 同一会话已从规范副本成功恢复时，多余的问题副本只保留告警日志，不再把会话
        // 报告为不可用。
        unavailable.retain(|session| {
            !durable
                .sessions
                .iter()
                .any(|loaded| loaded.session_id == session.session_id)
        });
        // current 指向的会话已被判定不可用时只清除指针；其余会话照常启动。
        if durable.current_session_id.as_ref().is_some_and(|current| {
            unavailable
                .iter()
                .any(|session| &session.session_id == current)
        }) {
            durable.current_session_id = None;
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

        // 回合事实已交给 SessionStore；事件缓存此后只保留游标与 accepted 记录，
        // 会话再次写入时才从事件目录重放常驻。
        for projection in event_cache.values_mut() {
            projection.release_canonical_turns();
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
        *self
            .unavailable_sessions
            .lock()
            .expect("unavailable sessions lock poisoned") = unavailable;
        Ok((durable, sidecars))
    }

    /// 最近一次加载中被判定不可用的会话。
    pub(crate) fn unavailable_sessions(&self) -> Vec<UnavailableSession> {
        self.unavailable_sessions
            .lock()
            .expect("unavailable sessions lock poisoned")
            .clone()
    }

    fn unavailable_session_from_path(
        path: &Path,
        workspace_id: Option<String>,
        error: &DaemonError,
    ) -> UnavailableSession {
        let storage_key = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        let session = UnavailableSession {
            session_id: magi_core::SessionId::new(Self::decode_session_storage_key(storage_key)),
            workspace_id,
            source_path: path.display().to_string(),
            reason: error.to_string(),
        };
        warn!(
            session_id = %session.session_id,
            path = %session.source_path,
            reason = %session.reason,
            "会话持久化文件损坏，已标记为不可用并保留原文件，其余会话继续启动"
        );
        session
    }

    /// `session_projection_file_name` 的逆变换：还原 `%XX` 转义的会话 ID。
    fn decode_session_storage_key(storage_key: &str) -> String {
        let bytes = storage_key.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%'
                && let Some(byte) = storage_key
                    .get(index + 1..index + 3)
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            {
                decoded.push(byte);
                index += 3;
                continue;
            }
            decoded.push(bytes[index]);
            index += 1;
        }
        String::from_utf8_lossy(&decoded).into_owned()
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
    /// projection 写入前崩溃；此时事件中的 accepted 事实会先把 session 恢复进内存，再调用
    /// 本方法确认该目录确实有归属。
    ///
    /// 没有任何归属的事件目录（典型来源：用户删除了工作区目录，其下会话的 projection 随之消失，
    /// 事件日志却留在全局 `session-events`）**不阻止启动**：整目录移到 `orphaned-session-events/`
    /// 隔离保留（只移动，不删除），启动日志给出告警。工作区本身按空白工作区加载。
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
        let event_cache = self
            .session_event_cache
            .lock()
            .expect("session event cache lock poisoned");
        // 不可用会话的事件目录是需要保留的原始数据，既不属于已恢复会话，也不能被当成
        // 孤立日志移走。
        let unavailable_roots = self
            .unavailable_sessions()
            .iter()
            .flat_map(|session| {
                [
                    self.session_event_root(&session.session_id),
                    PathBuf::from(&session.source_path),
                ]
            })
            .collect::<HashSet<_>>();
        let mut unowned = Vec::new();
        for entry in fs::read_dir(&event_parent)? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_dir() {
                return Err(DaemonError::internal(format!(
                    "canonical event 根目录包含非 session 目录: {}",
                    path.display()
                )));
            }
            if unavailable_roots.contains(&path) {
                continue;
            }
            let Some(session_id) = expected.get(&path) else {
                unowned.push(path);
                continue;
            };
            if !event_cache.contains_key(session_id) {
                return Err(DaemonError::internal(format!(
                    "canonical event 目录未包含在本次恢复结果中: {}",
                    path.display()
                )));
            }
        }
        drop(event_cache);
        self.quarantine_unowned_session_events(unowned)
    }

    /// 被隔离的无归属事件日志所在目录。
    fn orphaned_session_events_root(&self) -> PathBuf {
        self.state_root.join("orphaned-session-events")
    }

    /// 工作区目录复原后，把它的会话事件日志从隔离区移回原位。
    ///
    /// 会话投影（全局或工作区 `.magi/session-projections`）重新出现，而同名事件日志只在隔离区时，
    /// 说明这个会话的数据又回来了：必须在加载投影之前归位，否则投影会因缺少 canonical 事件而无法
    /// 加载。只移动、不覆盖；`session-events` 下已有同名目录时保持原样。
    fn restore_quarantined_session_events(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<(), DaemonError> {
        let quarantine_root = self.orphaned_session_events_root();
        if !quarantine_root.is_dir() {
            return Ok(());
        }
        let event_parent = self.state_root.join("session-events");
        let mut projection_roots = vec![self.session_projection_root()];
        projection_roots.extend(workspace_roots.iter().map(|(id, _)| {
            self.project_storage_root(id)
                .join(".magi")
                .join("session-projections")
        }));
        for projection_root in projection_roots {
            let Ok(entries) = fs::read_dir(&projection_root) else {
                continue;
            };
            for entry in entries {
                let path = entry?.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let Some(stem) = path.file_stem() else {
                    continue;
                };
                let quarantined = quarantine_root.join(stem);
                let target = event_parent.join(stem);
                if !quarantined.is_dir() || target.exists() {
                    continue;
                }
                fs::create_dir_all(&event_parent)?;
                fs::rename(&quarantined, &target)?;
                Self::sync_parent_directory(&quarantined);
                Self::sync_parent_directory(&target);
                tracing::info!(
                    session_events = %stem.to_string_lossy(),
                    "会话数据已复原，事件日志已从隔离区归位"
                );
            }
        }
        Ok(())
    }

    /// 把没有任何归属的事件目录整体移到 `orphaned-session-events/`（只移动，不删除）。
    fn quarantine_unowned_session_events(&self, unowned: Vec<PathBuf>) -> Result<(), DaemonError> {
        if unowned.is_empty() {
            return Ok(());
        }
        let quarantine_root = self.orphaned_session_events_root();
        fs::create_dir_all(&quarantine_root)?;
        for path in unowned {
            let Some(name) = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
            else {
                continue;
            };
            let mut target = quarantine_root.join(&name);
            if target.exists() {
                target = quarantine_root.join(format!("{name}-{}", magi_core::UtcMillis::now().0));
            }
            fs::rename(&path, &target)?;
            Self::sync_parent_directory(&path);
            Self::sync_parent_directory(&target);
            tracing::warn!(
                session_events = %name,
                quarantined_to = %target.display(),
                "会话事件日志没有对应的会话（工作区或会话数据已被删除），已隔离保留，不影响启动"
            );
        }
        Ok(())
    }

    pub(crate) fn workspace_projection_roots(
        &self,
    ) -> Result<HashMap<String, PathBuf>, DaemonError> {
        let state = self.load_workspace_durable_state()?;
        let mut roots = HashMap::new();
        for workspace in state.workspaces {
            // 工作区稳定身份只在启动恢复时校验；这里位于每次保存与会话导航的热路径。
            roots.insert(
                workspace.workspace_id.to_string(),
                workspace.native_root_path(),
            );
        }
        Ok(roots)
    }

    /// 启动时恢复中断的 session projection 事务，并校验 state layout 版本标记。
    ///
    /// 只接受当前版本布局：标记版本不一致直接拒绝启动；没有标记的 state 目录视为
    /// 全新状态并写入当前版本标记。
    pub(crate) fn verify_state_layout(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<(), DaemonError> {
        self.recover_session_projection_transaction(workspace_roots)?;
        let layout_path = self.state_layout_marker_path();
        if layout_path.exists() {
            let marker: StateLayoutMarker = self.read_json_strict(&layout_path)?;
            if matches!(marker.version, 2..=18) {
                // 升级写屏障，防止旧 daemon 重写新 Agent activation 与 Goal/Step。
                let backup = self
                    .state_root
                    .join(format!("state-layout.v{}.json", marker.version));
                if !backup.exists() {
                    self.write_json_atomically(backup, &marker)?;
                }
                return self.write_json_atomically(
                    layout_path,
                    &StateLayoutMarker {
                        version: STATE_LAYOUT_VERSION,
                    },
                );
            }
            return self.validate_state_layout_marker(&layout_path);
        }
        self.write_json_atomically(
            layout_path,
            &StateLayoutMarker {
                version: STATE_LAYOUT_VERSION,
            },
        )
    }

    fn state_layout_marker_path(&self) -> PathBuf {
        self.state_root.join("state-layout.json")
    }

    fn validate_state_layout_marker(&self, layout_path: &Path) -> Result<(), DaemonError> {
        let marker: StateLayoutMarker = self.read_json_strict(layout_path)?;
        if marker.version != STATE_LAYOUT_VERSION {
            return Err(DaemonError::internal(format!(
                "不支持的 state layout 版本: {}",
                marker.version
            )));
        }
        Ok(())
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
    ) -> Result<PathBuf, DaemonError> {
        let projection_root = match workspace_id {
            None => self.session_projection_root(),
            Some(workspace_id) => {
                if !workspace_roots.contains_key(workspace_id) {
                    return Err(DaemonError::internal(format!(
                        "session {session_id} 引用了未注册 workspace，拒绝回落到全局目录: {workspace_id}"
                    )));
                }
                self.project_storage_root(workspace_id)
                    .join(".magi")
                    .join("session-projections")
            }
        };
        Ok(projection_root.join(Self::session_projection_file_name(session_id)))
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
            .inputs
            .iter()
            .all(|input| input.session_id == session_id && input.validate_snapshot().is_ok())
            && snapshot
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
            if !json_values_semantically_equal(&cached, &authoritative) {
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

    fn build_session_projection_content(
        &self,
        durable: &SessionDurableState,
        sidecar: Option<SessionRuntimeSidecar>,
        workspace_roots: &HashMap<String, PathBuf>,
        session_id: &SessionId,
        event_cache: &mut HashMap<SessionId, SessionConversationProjection>,
        allow_canonical_event_advance: bool,
    ) -> Result<(PathBuf, String), DaemonError> {
        let path = self.session_projection_path(
            session_id,
            durable
                .sessions
                .first()
                .and_then(|session| session.workspace_id.as_deref()),
            workspace_roots,
        )?;
        let mut next_durable = durable.clone();
        let event_root = self.session_event_root(session_id);
        let mut event_projection = match event_cache.get(session_id) {
            Some(projection) => projection.clone(),
            None => SessionConversationProjection::load(&event_root, session_id)?,
        };
        // 已释放回合的会话自载入以来没有追加过任何事件（追加会先重放常驻），SessionStore 里的
        // 回合与事件重放结果等价，直接使用 `durable` 携带的回合，不必为了保存而重放整段历史。
        if event_projection.is_resident() {
            let memory_canonical =
                serde_json::to_value(&next_durable.canonical_turns).map_err(DaemonError::from)?;
            let authoritative = serde_json::to_value(event_projection.canonical_turns())
                .map_err(DaemonError::from)?;
            if !allow_canonical_event_advance
                && !json_values_semantically_equal(&memory_canonical, &authoritative)
            {
                return Err(DaemonError::internal(format!(
                    "session projection 不能反向生成 canonical 事实: {session_id}"
                )));
            }
            next_durable.canonical_turns = event_projection.canonical_turns().to_vec();
            next_durable.inputs = event_projection.inputs().to_vec();
            event_projection.touch();
        }
        let snapshot = SessionProjectionSnapshot {
            canonical_event_seq: event_projection.last_event_seq(),
            durable: next_durable,
            sidecar,
        };
        Self::validate_session_projection(&snapshot, &path)?;
        let content = serde_json::to_vec_pretty(&snapshot).map_err(DaemonError::from)?;
        let content = String::from_utf8(content).map_err(|error| {
            DaemonError::internal(format!("session projection 不是 UTF-8: {error}"))
        })?;
        event_cache.insert(session_id.clone(), event_projection);
        Ok((path, content))
    }

    fn save_session_projection_parts(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
        workspace_roots: &HashMap<String, PathBuf>,
        changed_session_ids: Option<&HashSet<SessionId>>,
        partial_snapshot: bool,
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
        let mut next_cache = if partial_snapshot {
            let snapshots = changed_session_ids
                .into_iter()
                .flat_map(|session_ids| session_ids.iter())
                .filter_map(|session_id| {
                    cache
                        .snapshots
                        .get(session_id)
                        .cloned()
                        .map(|snapshot| (session_id.clone(), snapshot))
                })
                .collect();
            SessionProjectionCache {
                snapshots,
                pending_removals: cache.pending_removals.clone(),
                pending_event_removals: cache.pending_event_removals.clone(),
                global: cache.global.clone(),
                app_meta: cache.app_meta.clone(),
                workspace_meta: cache.workspace_meta.clone(),
            }
        } else {
            cache.clone()
        };
        let mut next_event_cache = if partial_snapshot {
            changed_session_ids
                .into_iter()
                .flat_map(|session_ids| session_ids.iter())
                .filter_map(|session_id| {
                    event_cache
                        .get(session_id)
                        .cloned()
                        .map(|projection| (session_id.clone(), projection))
                })
                .collect()
        } else {
            event_cache.clone()
        };
        let mut writes = Vec::<SessionProjectionWrite>::new();
        let mut removals = Vec::<SessionProjectionRemoval>::new();
        let mut sidecar_by_session = HashMap::new();
        for sidecar in &sidecars.runtime_sidecars {
            if partial_snapshot
                && changed_session_ids
                    .is_some_and(|session_ids| !session_ids.contains(&sidecar.session_id))
            {
                continue;
            }
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
        if !partial_snapshot
            && let Some(orphan_sidecar_id) = sidecar_by_session
                .keys()
                .find(|session_id| !retained_ids.contains(*session_id))
        {
            return Err(DaemonError::internal(format!(
                "拒绝持久化无 session 归属的 sidecar: {orphan_sidecar_id}"
            )));
        }

        if !partial_snapshot
            && let Some(current_session_id) = durable.current_session_id.as_ref()
            && !retained_ids.contains(current_session_id)
        {
            return Err(DaemonError::internal(format!(
                "拒绝持久化悬空 session current 指针: {current_session_id}"
            )));
        }

        for session in &durable.sessions {
            if changed_session_ids
                .is_some_and(|session_ids| !session_ids.contains(&session.session_id))
            {
                continue;
            }
            let session_id = session.session_id.clone();
            let path = self.session_projection_path(
                &session_id,
                session.workspace_id.as_deref(),
                workspace_roots,
            )?;
            let previous = cache
                .snapshots
                .get(&session_id)
                .filter(|(previous_path, _)| previous_path == &path)
                .map(|(_, digest)| digest.clone());
            let session_durable = durable.durable_state_for_session(&session_id);
            let (built_path, content) = self.build_session_projection_content(
                &session_durable,
                sidecar_by_session.get(&session_id).cloned(),
                workspace_roots,
                &session_id,
                &mut next_event_cache,
                partial_snapshot,
            )?;
            debug_assert_eq!(built_path, path);
            let digest = ContentDigest::of(&content);
            if previous.as_ref() != Some(&digest) {
                writes.push(SessionProjectionWrite {
                    path: path.clone(),
                    content,
                });
            }
            if let Some((old_path, _)) = next_cache.snapshots.get(&session_id)
                && old_path != &path
            {
                return Err(DaemonError::internal(format!(
                    "session projection 归属路径发生变化，拒绝自动移动或删除: {} -> {}",
                    old_path.display(),
                    path.display()
                )));
            }
            next_cache
                .snapshots
                .insert(session_id, (path.clone(), digest));
        }

        if !partial_snapshot {
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
        }

        // 增量运行态 checkpoint 只拥有被标记 session 的 projection。它的 durable
        // 快照是在释放 SessionStore 锁后传入的，`current_session_id` 可能已经被用户
        // 导航更新；如果在这里照旧写全局 current 指针，就会用旧快照覆盖用户刚选中的
        // 会话。current 指针只由完整投影和导航事务写入，增量 checkpoint 保持原文件和
        // 缓存不变。
        if !partial_snapshot {
            let current_path = self.state_root.join("session-current.json");
            let current_content = serde_json::to_vec_pretty(&durable.current_session_id)
                .map_err(DaemonError::from)?;
            let current_content = String::from_utf8(current_content).map_err(|error| {
                DaemonError::internal(format!("session current state 不是 UTF-8: {error}"))
            })?;
            let current_digest = ContentDigest::of(&current_content);
            if next_cache
                .global
                .as_ref()
                .map(|(_, previous)| previous != &current_digest)
                .unwrap_or(true)
            {
                writes.push(SessionProjectionWrite {
                    path: current_path.clone(),
                    content: current_content,
                });
            }
            next_cache.global = Some((current_path, current_digest));
        }

        if !partial_snapshot {
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
            let app_meta_content =
                serde_json::to_vec_pretty(&app_meta).map_err(DaemonError::from)?;
            let app_meta_content = String::from_utf8(app_meta_content).map_err(|error| {
                DaemonError::internal(format!("session app meta 不是 UTF-8: {error}"))
            })?;
            let app_meta_digest = ContentDigest::of(&app_meta_content);
            if next_cache
                .app_meta
                .as_ref()
                .map(|(_, previous)| previous != &app_meta_digest)
                .unwrap_or(true)
            {
                writes.push(SessionProjectionWrite {
                    path: app_meta_path.clone(),
                    content: app_meta_content,
                });
            }
            next_cache.app_meta = Some((app_meta_path, app_meta_digest));
        }

        if !partial_snapshot {
            for workspace_id in workspace_roots.keys() {
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
                let path = self
                    .project_storage_root(workspace_id)
                    .join(".magi")
                    .join("session-workspace-meta.json");
                let content = serde_json::to_vec_pretty(&meta).map_err(DaemonError::from)?;
                let content = String::from_utf8(content).map_err(|error| {
                    DaemonError::internal(format!("workspace meta 不是 UTF-8: {error}"))
                })?;
                let digest = ContentDigest::of(&content);
                if next_cache
                    .workspace_meta
                    .get(workspace_id)
                    .map(|(_, previous)| previous != &digest)
                    .unwrap_or(true)
                {
                    writes.push(SessionProjectionWrite {
                        path: path.clone(),
                        content,
                    });
                }
                next_cache
                    .workspace_meta
                    .insert(workspace_id.clone(), (path, digest));
            }
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

        let transaction = SessionProjectionTransaction {
            schema_version: SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION,
            transaction_id: format!("session-projection-{}", magi_core::UtcMillis::now().0),
            writes,
            removals,
        };
        self.commit_session_projection_transaction_locked(&transaction, workspace_roots)?;
        self.ensure_state_layout_marker_locked()?;

        next_cache.pending_removals.clear();
        next_cache.pending_event_removals.clear();
        if partial_snapshot {
            for (session_id, snapshot) in next_cache.snapshots {
                cache.snapshots.insert(session_id, snapshot);
            }
            cache.pending_removals = next_cache.pending_removals;
            cache.pending_event_removals = next_cache.pending_event_removals;
            cache.global = next_cache.global;
            for (session_id, projection) in next_event_cache {
                event_cache.insert(session_id, projection);
            }
            trim_resident_event_projections(&mut event_cache);
        } else {
            *cache = next_cache;
            trim_resident_event_projections(&mut next_event_cache);
            *event_cache = next_event_cache;
        }

        Ok(())
    }

    fn ensure_state_layout_marker_locked(&self) -> Result<(), DaemonError> {
        let layout_path = self.state_layout_marker_path();
        if layout_path.exists() {
            return self.validate_state_layout_marker(&layout_path);
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

    pub(crate) fn task_store_projection_path(&self) -> PathBuf {
        self.state_root.join("task-store-projections")
    }

    /// 返回 canonical 事件中尚未完全收敛的 accepted 事实。
    ///
    /// accepted 事实与 canonical event 写在同一个事件事务里，session 侧在事件提交时即已
    /// durable；这里只按已提交的 task manifest 判断 task 侧是否已经 checkpoint。
    pub(crate) fn load_accepted_submissions(
        &self,
    ) -> Result<Vec<AcceptedSubmissionRecord>, DaemonError> {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let committed_generation =
            TaskStore::committed_projection_generation(&self.task_store_projection_path())?
                .unwrap_or(0);
        let mut records = self
            .event_accepted_submissions
            .lock()
            .expect("event accepted submission cache lock poisoned")
            .clone();
        for record in &mut records {
            // Conversation acceptance 没有 Task projection；其 canonical event 本身
            // 已经是完整 durable 事实，不能等待不存在的 task manifest。
            let Some(task) = record.task.as_ref() else {
                record.task_checkpointed = true;
                continue;
            };
            if !record.task_checkpointed
                && (committed_generation > record.task_projection_generation_at_acceptance
                    || TaskStore::committed_projection_contains_root(
                        &self.task_store_projection_path(),
                        &task.root_task_id,
                    )?)
            {
                record.task_checkpointed = true;
            }
        }
        records.retain(|record| !(record.session_checkpointed && record.task_checkpointed));
        Ok(records)
    }

    /// 在 repository 写锁内提交 task manifest。manifest 是 task projection 的唯一提交点；
    /// 提交后所有更早接纳的 task（包括已删除的 task）都已被该完整快照覆盖。
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
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        self.ensure_state_layout_marker_locked()?;
        Ok(TaskStore::checkpoint_snapshot_to_projection_directory(
            snapshot,
            &self.task_store_projection_path(),
        )?)
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
        // 只需要事件游标，已释放回合的投影同样有效。
        if !cache.contains_key(session_id) {
            let projection = SessionConversationProjection::load(&event_root, session_id)?;
            cache.insert(session_id.clone(), projection);
        }
        let next_sequence = cache.get(session_id).map_or(1, |projection| {
            projection.last_event_seq().saturating_add(1).max(1)
        });
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

    /// 审计/用量账本的追加写段目录（见 docs/durable-log-compaction-design.md §1）。
    pub(crate) fn audit_usage_ledger_path(&self) -> PathBuf {
        self.state_root.join("audit-usage-ledger")
    }

    pub(crate) fn load_audit_usage_ledger(&self) -> Result<AuditUsageLedgerSnapshot, DaemonError> {
        AuditUsageLedgerSnapshot::load_from_dir(
            &self.audit_usage_ledger_path(),
            magi_core::UtcMillis::now(),
        )
        .map_err(|error| {
            DaemonError::internal(format!(
                "审计/用量账本损坏，拒绝以空账本继续 {}: {error}",
                self.audit_usage_ledger_path().display()
            ))
        })
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
                "持久状态文件损坏，拒绝以空状态继续 {}: {error}",
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

/// 比较持久化 JSON 时使用 JSON 的数值语义，而不是数字的词法表示。
///
/// canonical event 与 projection 都来自同一份强类型 turn，但工具参数、结果和
/// metadata 允许嵌套任意 JSON。`1` 与 `1.0` 反序列化后会保留不同的
/// `serde_json::Number` 表示，却代表同一个 JSON 数值；用 `Value` 的直接相等判断
/// 会把正常的表示差异误判为事实损坏，导致 daemon 无法恢复既有配置和会话。
/// 事件日志仍是唯一事实源，调用方在校验后始终用重放结果覆盖缓存 projection。
fn json_values_semantically_equal(left: &serde_json::Value, right: &serde_json::Value) -> bool {
    match (left, right) {
        (serde_json::Value::Null, serde_json::Value::Null)
        | (serde_json::Value::Bool(_), serde_json::Value::Bool(_))
        | (serde_json::Value::String(_), serde_json::Value::String(_)) => left == right,
        (serde_json::Value::Number(left), serde_json::Value::Number(right)) => {
            left == right
                || match (left.as_f64(), right.as_f64()) {
                    (Some(left), Some(right)) => {
                        left.is_finite() && right.is_finite() && left == right
                    }
                    _ => false,
                }
        }
        (serde_json::Value::Array(left), serde_json::Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| json_values_semantically_equal(left, right))
        }
        (serde_json::Value::Object(left), serde_json::Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, left)| {
                    right
                        .get(key)
                        .is_some_and(|right| json_values_semantically_equal(left, right))
                })
        }
        _ => false,
    }
}

impl SessionEventWriter for StateRepository {
    fn append_session_transaction(
        &self,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
        inputs: &[SessionInputMutation],
        acceptance: Option<(&SessionAcceptanceRecord, Option<&Task>)>,
    ) -> DomainResult<()> {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");

        let Some((acceptance, task)) = acceptance else {
            return self
                .append_canonical_turn_transaction_locked(session_id, mutations, inputs, None);
        };
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
            task: task.cloned(),
            // canonical event 已经包含 session、timeline 和 sidecar，事件 segment 本身
            // 就是 session accepted 事实的 durable 提交点。
            session_checkpointed: true,
            // Conversation acceptance 没有待 checkpoint 的 Task；Task acceptance
            // 仍等待 task-store manifest 收敛。
            task_checkpointed: task.is_none(),
            task_projection_generation_at_acceptance,
        };
        self.append_canonical_turn_transaction_locked(
            session_id,
            mutations,
            inputs,
            Some(acceptance),
        )
    }
}

impl StateRepository {
    fn append_canonical_turn_transaction_locked(
        &self,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
        inputs: &[SessionInputMutation],
        acceptance: Option<AcceptedSubmissionRecord>,
    ) -> DomainResult<()> {
        let mut cache = self
            .session_event_cache
            .lock()
            .expect("session event cache lock poisoned");
        let event_root = self.session_event_root(session_id);
        // 已释放回合（或从未载入）的会话在写入前从事件目录重放常驻；已常驻的直接借用，
        // 不再为每个事件先整体克隆一份回合。
        if !cache
            .get(session_id)
            .is_some_and(SessionConversationProjection::is_resident)
        {
            let loaded =
                SessionConversationProjection::load(&event_root, session_id).map_err(|error| {
                    DomainError::Persistence {
                        message: error.to_string(),
                    }
                })?;
            cache.insert(session_id.clone(), loaded);
        }
        let projection = cache
            .get(session_id)
            .expect("projection was made resident above");
        let next = projection
            .append_session_transaction(
                &event_root,
                session_id,
                mutations,
                inputs,
                acceptance.clone(),
            )
            .map_err(|error| DomainError::Persistence {
                message: error.to_string(),
            })?;
        let mut next = next;
        next.touch();
        cache.insert(session_id.clone(), next);
        trim_resident_event_projections(&mut cache);
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

    fn persist_session_snapshot_incremental(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
        session_ids: &[SessionId],
    ) -> Result<Vec<SessionId>, DaemonError> {
        let started_at = std::time::Instant::now();
        // workspace 注册/删除本身已经在 workspace mutation 事务中持久化；session
        // 运行态 checkpoint 不再重复写入并 fsync 整个 workspaces.json。
        let workspace_roots = self
            .workspace_store
            .workspaces()
            .into_iter()
            .map(|workspace| {
                (
                    workspace.workspace_id.to_string(),
                    workspace.native_root_path(),
                )
            })
            .collect::<HashMap<_, _>>();
        // 工作区移除后仍可能收到原调用的结算更新。没有注册路径时保留其脏版本，
        // 既不回落到全局目录，也不让它阻断其他工作区的 checkpoint；重新注册后再补交。
        let writable_sessions = session_ids
            .iter()
            .filter(|id| {
                durable
                    .sessions
                    .iter()
                    .find(|session| &session.session_id == *id)
                    .is_none_or(|session| {
                        session
                            .workspace_id
                            .as_ref()
                            .is_none_or(|workspace| workspace_roots.contains_key(workspace))
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        if writable_sessions.is_empty() {
            return Ok(Vec::new());
        }
        let result = self
            .state_repository
            .save_session_projection_partial_state_with_roots(
                durable,
                sidecars,
                &writable_sessions,
                &workspace_roots,
            );
        if result.is_ok() {
            tracing::info!(
                target: "magi.performance",
                session_count = writable_sessions.len(),
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                stage = "session_projection_incremental_completed",
                "conversation response timing"
            );
        }
        result.map(|()| writable_sessions)
    }

    pub(crate) fn flush_runtime_sidecars(&self) -> Result<RuntimeSidecarFlushReport, DaemonError> {
        let worker_runtime_snapshot_flushed =
            self.worker_runtime
                .flush_durable_snapshot_with(|snapshot| {
                    self.state_repository.save_worker_runtime_snapshot(snapshot)
                })?;
        let session_sidecars_flushed = self.session_store.flush_execution_sidecar_snapshot_with(
            |durable, sidecars, session_ids| {
                self.persist_session_snapshot_incremental(durable, sidecars, session_ids)
            },
        )?;
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
        self.session_store.flush_execution_sidecar_snapshot_with(
            |durable, sidecars, session_ids| {
                self.persist_session_snapshot_incremental(durable, sidecars, session_ids)
            },
        )?;
        // 没有可提交的 sidecar 时也不回退全量快照；离线工作区保留脏版本，
        // canonical event 仍是业务事实的 durable 提交点。
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{
        AbsolutePath, ExecutionOwnership, MissionId, PlanId, PlanItem, PlanItemId, PlanItemStatus,
        PlanState, SessionId, SessionLifecycleStatus, TaskId, TaskKind, TaskStatus, ThreadId,
        UtcMillis, WorkerId, WorkspaceId,
    };
    use magi_session_store::{
        ActiveExecutionChain, ActiveExecutionDispatchContext, ActiveExecutionTurn,
        ActiveExecutionTurnItem, CanonicalTurnMutation, ExecutionThread, ExecutionThreadStatus,
        NotificationRecord, NotificationScope, SessionDurableState, SessionEventWriter,
        SessionPlan, SessionRecord, ThreadChatMessage, ThreadContextCheckpoint, TimelineEntry,
        TimelineEntryKind,
    };
    use std::collections::HashMap;

    #[test]
    fn semantic_json_comparison_accepts_numeric_representation_changes() {
        let cached = serde_json::json!({
            "clip": {"x": 0, "width": 1, "height": 1},
            "items": [1, 2]
        });
        let replayed = serde_json::json!({
            "clip": {"x": 0.0, "width": 1.0, "height": 1.0},
            "items": [1.0, 2.0]
        });
        assert!(json_values_semantically_equal(&cached, &replayed));
        assert!(!json_values_semantically_equal(
            &cached,
            &serde_json::json!({
                "clip": {"x": 0, "width": 1, "height": 2},
                "items": [1, 2]
            })
        ));
    }

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

    /// 测试夹具：把内存 SessionStore 已有的 canonical turn 写成事件日志。
    fn initialize_test_session_events(repository: &StateRepository, durable: &SessionDurableState) {
        let mut turns_by_session = HashMap::<SessionId, Vec<_>>::new();
        for turn in &durable.canonical_turns {
            turns_by_session
                .entry(turn.session_id.clone())
                .or_default()
                .push(turn.clone());
        }
        for (session_id, turns) in turns_by_session {
            let mutations = StateRepository::initial_canonical_mutations(turns);
            repository
                .append_session_transaction(&session_id, &mutations, &[], None)
                .expect("test canonical events should initialize");
        }
    }

    fn install_test_event_authority(repository: &StateRepository, session_store: &SessionStore) {
        initialize_test_session_events(repository, &session_store.durable_state());
        session_store.install_session_event_writer(Arc::new(repository.clone()));
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
    fn corrupted_session_projection_only_makes_that_session_unavailable() {
        let state_root = unique_temp_dir("magi-session-projection-corrupted");
        let repository = StateRepository::new(state_root.clone());
        let store = SessionStore::new();
        let healthy_id = SessionId::new("session-healthy");
        let corrupted_id = SessionId::new("session-corrupted");
        store
            .create_session(healthy_id.clone(), "healthy")
            .expect("healthy session should create");
        store
            .create_session(corrupted_id.clone(), "corrupted")
            .expect("corrupted session should create");
        store
            .persist_projection_with(|durable, sidecars| {
                repository.save_session_projection_state(durable, sidecars)
            })
            .expect("projections should save");
        // 模拟合并冲突或写入截断留下的损坏文件。
        let corrupted_path = repository
            .session_projection_root()
            .join(StateRepository::session_projection_file_name(&corrupted_id));
        fs::write(&corrupted_path, b"{\"version\":2,<<<<<<< HEAD").expect("corrupt projection");

        let restarted = StateRepository::new(state_root.clone());
        let (restored, _) = restarted
            .load_session_projections(&[])
            .expect("单个会话损坏不能阻止其余会话启动");

        assert_eq!(
            restored
                .sessions
                .iter()
                .map(|session| session.session_id.clone())
                .collect::<Vec<_>>(),
            vec![healthy_id]
        );
        let unavailable = restarted.unavailable_sessions();
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0].session_id, corrupted_id);
        assert_eq!(unavailable[0].workspace_id, None);
        assert_eq!(
            unavailable[0].source_path,
            corrupted_path.display().to_string()
        );
        assert!(
            unavailable[0]
                .reason
                .contains("解析 session projection 失败")
        );
        assert!(
            restored.current_session_id.as_ref() != Some(&corrupted_id),
            "current 不能指向不可用会话"
        );
        restarted
            .validate_session_event_log_coverage(&restored)
            .expect("不可用会话不影响事件日志校验");
        assert_eq!(
            fs::read(&corrupted_path).expect("损坏文件必须原地保留"),
            b"{\"version\":2,<<<<<<< HEAD"
        );

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn corrupted_durable_state_is_rejected_without_empty_state_recovery() {
        let state_root = unique_temp_dir("magi-durable-state-corrupted");
        let repository = StateRepository::new(state_root.clone());
        let path = state_root.join("workspaces.json");
        fs::write(&path, b"{not-json").expect("corrupted durable state should write");

        let error = repository
            .load_workspace_durable_state()
            .expect_err("corrupted durable state must reject recovery");
        assert!(error.to_string().contains("拒绝以空状态继续"));
        assert!(path.exists(), "损坏状态必须原地保留以便诊断和恢复");
        assert!(!path.with_file_name("workspaces.json.stale").exists());

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn duplicate_session_projection_does_not_block_recovery_or_delete_either_copy() {
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
            .create_session_for_workspace(
                session_id.clone(),
                "duplicate projection",
                Some("duplicate-root".to_string()),
            )
            .expect("session should create");
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("initial projection should save");

        let file_name = StateRepository::session_projection_file_name(&session_id);
        let canonical_path = repository
            .project_storage_root("duplicate-root")
            .join(".magi")
            .join("session-projections")
            .join(&file_name);
        let duplicate_path = repository.session_projection_root().join(&file_name);
        fs::create_dir_all(repository.session_projection_root())
            .expect("duplicate projection directory should create");
        fs::copy(&canonical_path, &duplicate_path).expect("matching duplicate should copy");
        let canonical_content = fs::read(&canonical_path).expect("canonical projection bytes");
        let duplicate_content = fs::read(&duplicate_path).expect("duplicate projection bytes");

        // 恢复时多余副本只让它自己被忽略：会话从规范副本恢复，两份文件都原样保留。
        let (restored, _) = repository
            .load_session_projections(&[("duplicate-root".to_string(), duplicate_root.clone())])
            .expect("duplicate projection must not block recovery");
        assert!(
            restored
                .sessions
                .iter()
                .any(|session| session.session_id == session_id)
        );
        assert!(repository.unavailable_sessions().is_empty());
        assert_eq!(
            fs::read(&canonical_path).expect("canonical projection remains"),
            canonical_content
        );
        assert_eq!(
            fs::read(&duplicate_path).expect("duplicate projection remains"),
            duplicate_content
        );

        let _ = fs::remove_dir_all(state_root);
        let _ = fs::remove_dir_all(duplicate_root);
    }

    #[test]
    fn changing_session_workspace_never_moves_projection_implicitly() {
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
        let error = session_store
            .persist_projection_with(|durable, sidecars| {
                repository.save_session_projection_state(durable, sidecars)
            })
            .expect_err("workspace ownership change must use an explicit migration transaction");
        assert!(error.to_string().contains("归属路径发生变化"));

        let new_path = workspace_root
            .join(".magi")
            .join("session-projections")
            .join(file_name);
        assert!(old_path.exists(), "原 projection 必须保留");
        assert!(!new_path.exists(), "失败事务不能写入新 projection");

        let _ = fs::remove_dir_all(state_root);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn partial_session_projection_checkpoint_only_rewrites_target_session() {
        let state_root = unique_temp_dir("magi-session-projection-partial");
        let repository = StateRepository::new(state_root.clone());
        let session_store = SessionStore::new();
        let first = SessionId::new("session-partial-first");
        let second = SessionId::new("session-partial-second");
        session_store
            .create_session(first.clone(), "partial first")
            .expect("first session should create");
        session_store
            .create_session(second.clone(), "partial second")
            .expect("second session should create");
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("initial full projection should save");

        let first_path = state_root
            .join("session-projections")
            .join(StateRepository::session_projection_file_name(&first));
        let second_path = state_root
            .join("session-projections")
            .join(StateRepository::session_projection_file_name(&second));
        let second_before = fs::read(&second_path).expect("second projection should exist");
        session_store
            .select_current_session(&first)
            .expect("first session should be selected before checkpoint snapshot");
        session_store.append_timeline_entry(
            first.clone(),
            TimelineEntryKind::AssistantMessage,
            "partial update",
        );
        // 模拟后台 checkpoint 在释放 SessionStore 快照锁后才开始写盘：此时它携带
        // 的 current 指针仍是 first，但用户已经切换到了 second 并完成了导航提交。
        let stale = session_store.durable_state();
        session_store
            .select_current_session(&second)
            .expect("user navigation should update current session");
        repository
            .save_session_navigation_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
                Some(&second),
            )
            .expect("new current session should persist before stale checkpoint");
        let sidecars = session_store.execution_sidecar_store_state();
        repository
            .save_session_projection_partial_state_with_roots(
                &stale,
                &sidecars,
                std::slice::from_ref(&first),
                &HashMap::new(),
            )
            .expect("partial projection should save");

        let first_after = fs::read_to_string(&first_path).expect("first projection should exist");
        assert!(first_after.contains("partial update"));
        assert_eq!(
            fs::read(&second_path).expect("second projection should remain"),
            second_before,
            "未变化 session 的 projection 不应被重写"
        );
        let current: Option<SessionId> = serde_json::from_str(
            &fs::read_to_string(state_root.join("session-current.json"))
                .expect("current pointer should remain readable"),
        )
        .expect("current pointer should parse");
        assert_eq!(
            current,
            Some(second),
            "旧的增量 checkpoint 不得覆盖用户刚提交的 current session"
        );

        let _ = fs::remove_dir_all(state_root);
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
    fn background_checkpoint_cannot_overwrite_a_session_rename_or_deletion() {
        use std::{sync::mpsc, thread, time::Duration};

        for delete in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let repository = StateRepository::new(root.path().to_path_buf());
            let (store, turn_id, _) = accepted_session_store("checkpoint-race", None, 45);
            let session = SessionId::new("checkpoint-race");
            install_test_event_authority(&repository, &store);
            store
                .persist_projection_with(|durable, sidecars| {
                    repository.save_session_projection_state(durable, sidecars)
                })
                .unwrap();
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let checkpoint_store = store.clone();
            let checkpoint_repository = repository.clone();
            let checkpoint = thread::spawn(move || {
                checkpoint_store
                    .flush_execution_sidecar_snapshot_with(|durable, sidecars, ids| {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        checkpoint_repository.save_session_projection_partial_state_with_roots(
                            durable,
                            sidecars,
                            ids,
                            &HashMap::new(),
                        )?;
                        Ok::<_, DaemonError>(ids.to_vec())
                    })
                    .unwrap()
            });
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

            // checkpoint 的磁盘 IO 不得持有 canonical 或 state 锁：新工具结果仍可
            // 提交事件，投影 builder 会从日志补齐它，而不是覆盖成旧快照。
            append_test_tool_item(&store, &session, &turn_id, "tool-during-checkpoint", 2);
            assert_eq!(store.canonical_turns_for_session(&session).len(), 1);
            let (finished_tx, finished_rx) = mpsc::channel();
            let mutation_store = store.clone();
            let mutation_session = session.clone();
            let mutation_repository = repository.clone();
            let mutation = thread::spawn(move || {
                let persist =
                    |durable: &SessionDurableState,
                     sidecars: &SessionExecutionSidecarStoreState| {
                        mutation_repository.save_session_projection_state_for_sessions(
                            durable,
                            sidecars,
                            std::slice::from_ref(&mutation_session),
                        )
                    };
                if delete {
                    mutation_store
                        .delete_session_with_persistence(&mutation_session, None, persist)
                        .unwrap();
                } else {
                    mutation_store
                        .rename_session_with_persistence(&mutation_session, "renamed", persist)
                        .unwrap();
                }
                finished_tx.send(()).unwrap();
            });
            let overtook_checkpoint = finished_rx.recv_timeout(Duration::from_millis(100)).is_ok();
            release_tx.send(()).unwrap();
            assert!(checkpoint.join().unwrap());
            mutation.join().unwrap();

            let (restored, _) = StateRepository::new(root.path().to_path_buf())
                .load_session_projections(&[])
                .unwrap();
            if delete {
                assert!(
                    restored.sessions.is_empty(),
                    "旧 checkpoint 不得恢复已删除会话"
                );
                assert!(!repository.session_event_root(&session).exists());
            } else {
                assert_eq!(
                    restored.sessions[0].title, "renamed",
                    "旧 checkpoint 不得撤销重命名"
                );
                assert!(
                    restored.canonical_turns[0]
                        .items
                        .iter()
                        .any(|item| { item.item_id == "tool-during-checkpoint" })
                );
            }
            assert!(!overtook_checkpoint, "会话投影事务必须按快照捕获顺序提交");
        }
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
    fn session_navigation_only_writes_pointer_and_missing_target_projection() {
        let state_root = unique_temp_dir("magi-session-navigation-targeted");
        let repository = StateRepository::new(state_root.clone());
        let store = SessionStore::new();
        let first_session_id = SessionId::new("session-navigation-first");
        let second_session_id = SessionId::new("session-navigation-second");
        store
            .create_session(first_session_id.clone(), "first")
            .expect("first session should create");
        repository
            .save_session_projection_state(
                &store.durable_state(),
                &store.execution_sidecar_store_state(),
            )
            .expect("first session should persist");
        let first_projection_path = state_root.join("session-projections").join(
            StateRepository::session_projection_file_name(&first_session_id),
        );
        let first_projection_before =
            fs::read(&first_projection_path).expect("first projection should exist");

        store
            .create_session(second_session_id.clone(), "second")
            .expect("second session should create");
        repository
            .save_session_navigation_state(
                &store.durable_state(),
                &store.execution_sidecar_store_state(),
                Some(&second_session_id),
            )
            .expect("new target navigation should persist");
        let second_projection_path = state_root.join("session-projections").join(
            StateRepository::session_projection_file_name(&second_session_id),
        );
        assert!(second_projection_path.exists());
        let second_projection_before =
            fs::read(&second_projection_path).expect("second projection should exist");
        assert_eq!(
            fs::read(&first_projection_path).expect("first projection should remain readable"),
            first_projection_before
        );

        store
            .select_current_session(&first_session_id)
            .expect("first session should select");
        repository
            .save_session_navigation_state(
                &store.durable_state(),
                &store.execution_sidecar_store_state(),
                Some(&first_session_id),
            )
            .expect("existing target navigation should persist");
        assert_eq!(
            fs::read(&second_projection_path).expect("second projection should remain readable"),
            second_projection_before
        );

        store.clear_current_session();
        repository
            .save_session_navigation_state(
                &store.durable_state(),
                &store.execution_sidecar_store_state(),
                None,
            )
            .expect("draft navigation should persist");
        let current: Option<SessionId> = serde_json::from_str(
            &fs::read_to_string(state_root.join("session-current.json"))
                .expect("current pointer should exist"),
        )
        .expect("current pointer should parse");
        assert_eq!(current, None);
        assert_eq!(
            fs::read(&first_projection_path).expect("first projection should remain stable"),
            first_projection_before
        );

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

    #[test]
    fn removed_workspace_does_not_block_other_session_checkpoints_and_readd_flushes_pending() {
        let root = unique_temp_dir("magi-removed-workspace-checkpoint");
        let repository = StateRepository::new(root.join("state"));
        let workspace_root = root.join("workspace");
        fs::create_dir_all(&workspace_root).unwrap();
        let workspace = WorkspaceId::new("removed-workspace");
        let workspaces = Arc::new(WorkspaceStore::new());
        workspaces
            .register(
                workspace.clone(),
                AbsolutePath::new(workspace_root.to_string_lossy()),
            )
            .unwrap();
        repository
            .save_workspace_durable_state(&workspaces.durable_state())
            .unwrap();
        let store = Arc::new(SessionStore::new());
        let offline = SessionId::new("checkpoint-offline");
        let healthy = SessionId::new("checkpoint-healthy");
        store
            .create_session_for_workspace(offline.clone(), "offline", Some(workspace.to_string()))
            .unwrap();
        store.create_session(healthy.clone(), "healthy").unwrap();
        for id in [&offline, &healthy] {
            store.bind_execution_ownership(id.clone(), ExecutionOwnership::default());
        }
        let persistence = RuntimeSidecarPersistence::new(
            repository.clone(),
            store.clone(),
            workspaces.clone(),
            WorkerRuntime::new(),
        );
        assert!(
            persistence
                .flush_runtime_sidecars()
                .unwrap()
                .session_sidecars_flushed
        );
        let offline_path = repository
            .project_storage_root(workspace.as_str())
            .join(".magi/session-projections")
            .join(StateRepository::session_projection_file_name(&offline));
        let healthy_path = repository
            .session_projection_root()
            .join(StateRepository::session_projection_file_name(&healthy));
        let old_offline = fs::read(&offline_path).unwrap();
        let old_healthy = fs::read(&healthy_path).unwrap();
        workspaces.deregister(&workspace).unwrap();
        repository
            .save_workspace_durable_state(&workspaces.durable_state())
            .unwrap();
        for id in [&offline, &healthy] {
            let mut sidecar = store.runtime_sidecar(id).unwrap();
            sidecar.updated_at = UtcMillis(sidecar.updated_at.0 + 1000);
            store.upsert_runtime_sidecar(sidecar);
        }
        // 原来的全批提交会在离线工作区失败，健康会话的文件也无法更新。
        assert!(
            store
                .persist_projection_with(
                    |durable, sidecars| repository.save_session_projection_state(durable, sidecars)
                )
                .is_err()
        );
        assert!(
            persistence
                .flush_runtime_sidecars()
                .unwrap()
                .session_sidecars_flushed
        );
        assert_eq!(fs::read(&offline_path).unwrap(), old_offline);
        assert_ne!(fs::read(&healthy_path).unwrap(), old_healthy);
        assert_eq!(
            store.pending_execution_sidecar_session_ids(),
            vec![offline.clone()]
        );
        assert!(
            !persistence
                .flush_runtime_sidecars()
                .unwrap()
                .session_sidecars_flushed
        );
        assert!(
            !repository
                .session_projection_root()
                .join(StateRepository::session_projection_file_name(&offline))
                .exists()
        );
        workspaces
            .register(
                workspace.clone(),
                AbsolutePath::new(workspace_root.to_string_lossy()),
            )
            .unwrap();
        repository
            .save_workspace_durable_state(&workspaces.durable_state())
            .unwrap();
        assert!(
            persistence
                .flush_runtime_sidecars()
                .unwrap()
                .session_sidecars_flushed
        );
        assert!(store.pending_execution_sidecar_session_ids().is_empty());
        assert_ne!(fs::read(&offline_path).unwrap(), old_offline);
        let restarted = StateRepository::new(root.join("state"));
        let workspace_roots = workspaces
            .workspaces()
            .into_iter()
            .map(|workspace| {
                (
                    workspace.workspace_id.to_string(),
                    workspace.native_root_path(),
                )
            })
            .collect::<Vec<_>>();
        let (durable, sidecars) = restarted
            .load_session_projections(&workspace_roots)
            .unwrap();
        let restored = SessionStore::from_persisted_parts(durable, sidecars).unwrap();
        assert_eq!(
            serde_json::to_value(restored.runtime_sidecar(&offline)).unwrap(),
            serde_json::to_value(store.runtime_sidecar(&offline)).unwrap()
        );
        assert_eq!(
            serde_json::to_value(restored.runtime_sidecar(&healthy)).unwrap(),
            serde_json::to_value(store.runtime_sidecar(&healthy)).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
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
        let workspace_store = WorkspaceStore::new();
        workspace_store
            .register(
                WorkspaceId::new("workspace-persisted"),
                AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
            )
            .expect("workspace should register");
        repository
            .save_workspace_durable_state(&workspace_store.durable_state())
            .expect("workspace registry should persist");
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
                kind: Default::default(),
            }],
            timeline: vec![TimelineEntry {
                entry_id: "timeline-persisted-user".to_string(),
                session_id: session_id.clone(),
                kind: TimelineEntryKind::UserMessage,
                message: "恢复后的用户消息".to_string(),
                occurred_at: now,
            }],
            canonical_turns: vec![],
            inputs: vec![],
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
                    input_request_id: None,
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
                    input_request_id: None,
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
    fn input_only_transaction_restores_from_events_over_a_stale_session_projection() {
        let root = unique_temp_dir("magi-input-event-restore");
        let repository = StateRepository::new(root.clone());
        repository.verify_state_layout(&[]).unwrap();
        let store = SessionStore::new();
        let session = SessionId::new("session-pending-input-restore");
        store.create_session(session.clone(), "pending").unwrap();
        store.install_session_event_writer(Arc::new(repository.clone()));
        repository
            .save_session_projection_state(
                &store.durable_state(),
                &store.execution_sidecar_store_state(),
            )
            .unwrap();
        let input = magi_session_store::SessionInput {
            input_id: "input-restore".into(),
            session_id: session.clone(),
            request_id: "request-restore".into(),
            user_message_id: "user-restore".into(),
            request_fingerprint: "fingerprint-restore".into(),
            accepted_at: UtcMillis(50),
            revision: 1,
            state: magi_session_store::SessionInputState::Pending,
            attached_turn_id: None,
            consumption: None,
            interruption: None,
            payload: serde_json::json!({"request":"frozen"}),
        };
        store.commit_input(input.clone(), None).unwrap();
        // 只保留写入前的 Session projection，销毁内存并从新 repository 重放。
        let restarted = StateRepository::new(root.clone());
        let (restored, _) = restarted.load_session_projections(&[]).unwrap();
        assert_eq!(restored.inputs, vec![input]);
        assert!(restored.canonical_turns.is_empty());
        assert!(restarted.unavailable_sessions().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replacement_barrier_and_settlement_replay_from_canonical_log_without_checkpoint() {
        let root = unique_temp_dir("magi-replacement-barrier-replay");
        let repository = StateRepository::new(root.clone());
        repository.verify_state_layout(&[]).unwrap();
        let (store, turn, task) = accepted_session_store("replacement-replay", None, 10);
        let session = SessionId::new("replacement-replay");
        install_test_event_authority(&repository, &store);
        repository
            .save_session_projection_state(
                &store.durable_state(),
                &store.execution_sidecar_store_state(),
            )
            .unwrap();
        let pending = magi_session_store::SessionInput {
            input_id: "replacement-replay-input".into(),
            session_id: session.clone(),
            request_id: "replacement-replay-request".into(),
            user_message_id: "replacement-replay-user".into(),
            request_fingerprint: "replacement-fingerprint".into(),
            accepted_at: UtcMillis(20),
            revision: 1,
            state: magi_session_store::SessionInputState::Pending,
            attached_turn_id: None,
            consumption: None,
            interruption: Some(magi_session_store::InputInterruption {
                turn_id: turn,
                root_task_id: Some(task),
                settled_at: None,
                error: None,
            }),
            payload: serde_json::json!({"request":"frozen"}),
        };
        store.commit_input(pending.clone(), None).unwrap();
        // 输入提交后未做快照：重启必须从同一个 Session 日志恢复屏障。
        let restarted = StateRepository::new(root.clone());
        let (durable, sidecar) = restarted.load_session_projections(&[]).unwrap();
        let store = SessionStore::from_persisted_parts(durable, sidecar).unwrap();
        assert_eq!(
            store.input_for_request(&pending.request_id),
            Some(pending.clone())
        );
        store.install_session_event_writer(Arc::new(restarted));
        store
            .interrupt_current_turn_by_daemon_restart(&session)
            .unwrap();
        let mut settled = pending;
        settled.revision = 2;
        settled.interruption.as_mut().unwrap().settled_at = Some(UtcMillis(30));
        store.commit_input(settled.clone(), Some(1)).unwrap();
        let final_repository = StateRepository::new(root.clone());
        let (durable, sidecar) = final_repository.load_session_projections(&[]).unwrap();
        let store = SessionStore::from_persisted_parts(durable, sidecar).unwrap();
        assert_eq!(store.input_for_request(&settled.request_id), Some(settled));
        assert!(
            store.canonical_turns_for_session(&session)[0]
                .status
                .is_terminal()
        );
        assert!(final_repository.unavailable_sessions().is_empty());
        fs::remove_dir_all(root).unwrap();
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

        magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
            .set_status_domain(&session_id, Some(turn_id.as_str()), "completed")
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
    fn canonical_event_log_rebuilds_stale_sidecar_even_when_event_cursor_matches() {
        let state_root = unique_temp_dir("magi-canonical-sidecar-authority");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, turn_id, _) =
            accepted_session_store("canonical-sidecar-authority", None, 51);
        let session_id = SessionId::new("canonical-sidecar-authority");
        install_test_event_authority(&repository, &session_store);

        // 先持久化仅含 accepted turn 的 sidecar。随后 canonical 事件新增 item 并收口
        // turn；模拟进程在 sidecar flush 前退出。此时 snapshot 的 canonical 游标可以
        // 已经最新，但 sidecar 仍是更早的执行缓存。
        let stale_sidecar = session_store
            .execution_sidecar_store_state()
            .runtime_sidecar(&session_id)
            .expect("accepted turn should have a sidecar");
        magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
            .append_item_sidecar(
                &session_id,
                Some(turn_id.as_str()),
                ActiveExecutionTurnItem {
                    item_id: "turn-item-after-sidecar-checkpoint".to_string(),
                    item_seq: 1,
                    kind: "assistant_stream".to_string(),
                    status: "running".to_string(),
                    source: "orchestrator".to_string(),
                    title: None,
                    content: Some("canonical event is authoritative".to_string()),
                    task_id: None,
                    worker_id: None,
                    role_id: None,
                    tool_call_id: None,
                    tool_name: None,
                    tool_status: None,
                    tool_arguments: None,
                    tool_result: None,
                    tool_error: None,
                    request_id: None,
                    user_message_id: None,
                    placeholder_message_id: None,
                    metadata: HashMap::new(),
                    timeline_entry_id: None,
                    source_thread_id: ThreadId::new(format!("thread-orchestrator-{session_id}")),
                },
            )
            .expect("canonical item should append");
        magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
            .set_status_domain(&session_id, Some(turn_id.as_str()), "completed")
            .expect("turn should complete");
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("terminal canonical projection should persist");

        let projection_path = repository
            .session_projection_root()
            .join(StateRepository::session_projection_file_name(&session_id));
        let mut snapshot: SessionProjectionSnapshot =
            serde_json::from_slice(&fs::read(&projection_path).expect("projection should read"))
                .expect("projection should parse");
        assert_eq!(
            snapshot.canonical_event_seq,
            SessionConversationProjection::load(
                &repository.session_event_root(&session_id),
                &session_id
            )
            .expect("event projection should load")
            .last_event_seq(),
            "fixture must prove that the durable event cursor is already current"
        );
        snapshot.sidecar = Some(stale_sidecar);
        repository
            .write_json_atomically(projection_path, &snapshot)
            .expect("stale sidecar fixture should persist");

        let (durable, sidecars) = StateRepository::new(state_root.clone())
            .load_session_projections(&[])
            .expect("canonical event must rebuild a stale sidecar regardless of cursor equality");
        let canonical = durable
            .canonical_turns
            .iter()
            .find(|turn| turn.session_id == session_id && turn.turn_id == turn_id)
            .expect("canonical turn should restore");
        assert_eq!(
            canonical.status,
            magi_session_store::CanonicalTurnStatus::Completed
        );
        let recovered_turn = sidecars
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("rebuilt sidecar turn should restore");
        assert_eq!(recovered_turn.status, "completed");
        assert_eq!(recovered_turn.items.len(), canonical.items.len());
        assert_eq!(
            recovered_turn.items[0].item_id,
            "turn-item-after-sidecar-checkpoint"
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

        let restarted = StateRepository::new(state_root.clone());
        let (restored, _) = restarted
            .load_session_projections(&[])
            .expect("缺失事件日志只让该会话不可用");
        assert!(
            restored.sessions.is_empty(),
            "v2 projection must not recreate missing canonical facts"
        );
        let unavailable = restarted.unavailable_sessions();
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0].session_id, session_id);
        assert!(unavailable[0].reason.contains("canonical event 游标超前"));
        assert!(!repository.session_event_root(&session_id).exists());

        let _ = fs::remove_dir_all(state_root);
    }

    fn migrate_legacy_project_fixture(repository: &StateRepository, id: &str, root: &Path) {
        let host = magi_core::HostPath::from_path(root.to_path_buf());
        let legacy = serde_json::json!({
            "active_workspace_id": id, "workspaces": [{
                "workspaceId": id, "name": "legacy", "rootPath": host.display_string(),
                "rootPathRef": host.to_path_ref().as_str(), "worktreeRoot": null,
                "status": "Registered", "createdAt": 1, "updatedAt": 1
            }], "worktree_allocations": [], "snapshots": []
        });
        repository
            .write_json_atomically(repository.state_root.join("workspaces.json"), &legacy)
            .unwrap();
        repository
            .verify_state_layout(&[(id.into(), root.to_path_buf())])
            .unwrap();
        repository.migrate_project_storage().unwrap();
    }

    #[test]
    fn detached_workspace_projection_imports_into_an_empty_event_state_root() {
        let source_state_root = unique_temp_dir("magi-workspace-import-source");
        let target_state_root = unique_temp_dir("magi-workspace-import-target");
        let workspace_root = unique_temp_dir("magi-workspace-import-project");
        let source_repository = StateRepository::new(source_state_root.clone());
        let (session_store, turn_id, _) =
            accepted_session_store("workspace-import-session", Some("workspace-import"), 56);
        let session_id = SessionId::new("workspace-import-session");
        install_test_event_authority(&source_repository, &session_store);
        magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
            .set_status_domain(&session_id, Some(turn_id.as_str()), "completed")
            .expect("workspace import turn should complete");
        let source_events = SessionConversationProjection::load(
            &source_repository.session_event_root(&session_id),
            &session_id,
        )
        .expect("source events should load");
        let durable = session_store
            .durable_state()
            .durable_state_for_session(&session_id);
        let sidecar = session_store
            .execution_sidecar_store_state()
            .runtime_sidecar(&session_id);
        let projection_path = workspace_root
            .join(".magi")
            .join("session-projections")
            .join(StateRepository::session_projection_file_name(&session_id));
        source_repository
            .write_json_atomically(
                projection_path.clone(),
                &SessionProjectionSnapshot {
                    canonical_event_seq: source_events.last_event_seq(),
                    durable: durable.clone(),
                    sidecar,
                },
            )
            .expect("portable workspace projection should persist");
        let original_projection_content =
            fs::read(&projection_path).expect("portable projection bytes");

        let target_repository = StateRepository::new(target_state_root.clone());
        let identity = magi_workspace::resolve_or_create_workspace_identity(
            &workspace_root,
            WorkspaceId::new("unused-new-identity"),
        )
        .unwrap();
        assert_eq!(identity.as_str(), "workspace-import");
        let projects = WorkspaceStore::new();
        projects
            .register_native_path(identity, workspace_root.clone())
            .unwrap();
        target_repository
            .save_workspace_durable_state(&projects.durable_state())
            .unwrap();
        target_repository
            .verify_state_layout(&[("workspace-import".into(), workspace_root.clone())])
            .unwrap();
        target_repository.migrate_project_storage().unwrap();
        let (restored, _) = target_repository
            .load_session_projections(&[("workspace-import".to_string(), workspace_root.clone())])
            .expect("detached workspace projection should import into an empty event root");
        assert_eq!(restored.canonical_turns, durable.canonical_turns);
        let imported_events = SessionConversationProjection::load(
            &target_repository.session_event_root(&session_id),
            &session_id,
        )
        .expect("imported events should load");
        assert_eq!(imported_events.canonical_turns(), durable.canonical_turns);
        let rewritten: SessionProjectionSnapshot = serde_json::from_slice(
            &fs::read(&projection_path).expect("rewritten projection should read"),
        )
        .expect("rewritten projection should parse");
        assert_eq!(
            rewritten.canonical_event_seq,
            imported_events.last_event_seq()
        );
        let imported_projection_content = fs::read(&projection_path).expect("projection bytes");
        assert_eq!(
            imported_projection_content, original_projection_content,
            "匹配的 canonical 游标不应重写 project projection"
        );
        let imported_event_root = target_repository.session_event_root(&session_id);
        let imported_event_content = fs::read(
            fs::read_dir(&imported_event_root)
                .expect("event root")
                .next()
                .expect("event segment")
                .expect("event entry")
                .path(),
        )
        .expect("event bytes");
        assert!(
            !target_repository
                .session_projection_transaction_path()
                .exists()
        );

        let restarted_repository = StateRepository::new(target_state_root.clone());
        let (restarted, _) = restarted_repository
            .load_session_projections(&[("workspace-import".to_string(), workspace_root.clone())])
            .expect("workspace import should be idempotent after restart");
        assert_eq!(restarted.canonical_turns, durable.canonical_turns);
        assert_eq!(
            fs::read(&projection_path).expect("restarted projection bytes"),
            imported_projection_content
        );
        assert_eq!(
            fs::read(
                fs::read_dir(&imported_event_root)
                    .expect("restarted event root")
                    .next()
                    .expect("restarted event segment")
                    .expect("restarted event entry")
                    .path()
            )
            .expect("restarted event bytes"),
            imported_event_content
        );

        let _ = fs::remove_dir_all(source_state_root);
        let _ = fs::remove_dir_all(target_state_root);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn workspace_projection_identity_mismatch_is_unavailable_and_never_moves_source() {
        let source_state_root = unique_temp_dir("magi-workspace-mismatch-source");
        let target_state_root = unique_temp_dir("magi-workspace-mismatch-target");
        let workspace_root = unique_temp_dir("magi-workspace-mismatch-project");
        let source_repository = StateRepository::new(source_state_root.clone());
        let (session_store, _, _) =
            accepted_session_store("workspace-mismatch-session", Some("workspace-original"), 57);
        let session_id = SessionId::new("workspace-mismatch-session");
        install_test_event_authority(&source_repository, &session_store);
        let source_events = SessionConversationProjection::load(
            &source_repository.session_event_root(&session_id),
            &session_id,
        )
        .expect("source events should load");
        let projection_path = workspace_root
            .join(".magi")
            .join("session-projections")
            .join(StateRepository::session_projection_file_name(&session_id));
        source_repository
            .write_json_atomically(
                projection_path.clone(),
                &SessionProjectionSnapshot {
                    canonical_event_seq: source_events.last_event_seq(),
                    durable: session_store
                        .durable_state()
                        .durable_state_for_session(&session_id),
                    sidecar: session_store
                        .execution_sidecar_store_state()
                        .runtime_sidecar(&session_id),
                },
            )
            .expect("mismatched projection fixture should persist");
        let original_content = fs::read(&projection_path).expect("source projection bytes");

        let target_repository = StateRepository::new(target_state_root.clone());
        migrate_legacy_project_fixture(&target_repository, "workspace-registered", &workspace_root);
        let (restored, _) = target_repository
            .load_session_projections(&[(
                "workspace-registered".to_string(),
                workspace_root.clone(),
            )])
            .expect("workspace identity mismatch only makes that session unavailable");
        assert!(restored.sessions.is_empty());
        let unavailable = target_repository.unavailable_sessions();
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0].session_id, session_id);
        assert_eq!(
            unavailable[0].workspace_id.as_deref(),
            Some("workspace-registered")
        );
        assert!(unavailable[0].reason.contains("归属与扫描根不一致"));
        assert_eq!(
            fs::read(&projection_path).expect("source projection must remain"),
            original_content
        );
        assert!(!target_repository.session_event_root(&session_id).exists());
        assert!(
            !target_repository
                .session_projection_root()
                .join(StateRepository::session_projection_file_name(&session_id))
                .exists()
        );

        let _ = fs::remove_dir_all(source_state_root);
        let _ = fs::remove_dir_all(target_state_root);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn interrupted_workspace_event_import_recovers_from_durable_transaction() {
        let source_state_root = unique_temp_dir("magi-workspace-import-journal-source");
        let target_state_root = unique_temp_dir("magi-workspace-import-journal-target");
        let workspace_root = unique_temp_dir("magi-workspace-import-journal-project");
        let source_repository = StateRepository::new(source_state_root.clone());
        let (session_store, turn_id, _) = accepted_session_store(
            "workspace-import-journal-session",
            Some("workspace-import-journal"),
            59,
        );
        let session_id = SessionId::new("workspace-import-journal-session");
        install_test_event_authority(&source_repository, &session_store);
        magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
            .set_status_domain(&session_id, Some(turn_id.as_str()), "completed")
            .expect("journal import turn should complete");
        let source_events = SessionConversationProjection::load(
            &source_repository.session_event_root(&session_id),
            &session_id,
        )
        .expect("source events should load");
        let mut snapshot = SessionProjectionSnapshot {
            canonical_event_seq: source_events.last_event_seq(),
            durable: session_store
                .durable_state()
                .durable_state_for_session(&session_id),
            sidecar: session_store
                .execution_sidecar_store_state()
                .runtime_sidecar(&session_id),
        };
        let projection_path = workspace_root
            .join(".magi")
            .join("session-projections")
            .join(StateRepository::session_projection_file_name(&session_id));
        source_repository
            .write_json_atomically(projection_path.clone(), &snapshot)
            .expect("portable projection should persist");

        let target_repository = StateRepository::new(target_state_root.clone());
        let event_root = target_repository.session_event_root(&session_id);
        let mutations =
            StateRepository::initial_canonical_mutations(snapshot.durable.canonical_turns.clone());
        let prepared = SessionConversationProjection::load(&event_root, &session_id)
            .expect("empty target event projection")
            .prepare_transaction_write(&event_root, &session_id, &mutations, &[])
            .expect("event import transaction should prepare");
        snapshot.canonical_event_seq = prepared.projection.last_event_seq();
        let projection_content =
            serde_json::to_string_pretty(&snapshot).expect("projection transaction content");
        let transaction = SessionProjectionTransaction {
            schema_version: SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION,
            transaction_id: "workspace-import-interrupted".to_string(),
            writes: vec![
                SessionProjectionWrite {
                    path: prepared.path,
                    content: prepared.content,
                },
                SessionProjectionWrite {
                    path: projection_path.clone(),
                    content: projection_content,
                },
            ],
            removals: Vec::new(),
        };
        target_repository
            .write_json_atomically(
                target_repository.session_projection_transaction_path(),
                &transaction,
            )
            .expect("durable transaction journal should persist");
        assert!(!event_root.exists());

        let restarted_repository = StateRepository::new(target_state_root.clone());
        migrate_legacy_project_fixture(
            &restarted_repository,
            "workspace-import-journal",
            &workspace_root,
        );
        let (restored, _) = restarted_repository
            .load_session_projections(&[(
                "workspace-import-journal".to_string(),
                workspace_root.clone(),
            )])
            .expect("startup should finish interrupted import transaction");
        assert_eq!(
            restored.canonical_turns,
            session_store.durable_state().canonical_turns
        );
        assert!(event_root.exists());
        assert!(
            !restarted_repository
                .session_projection_transaction_path()
                .exists()
        );

        let _ = fs::remove_dir_all(source_state_root);
        let _ = fs::remove_dir_all(target_state_root);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn unknown_workspace_projection_never_falls_back_to_global_storage() {
        let state_root = unique_temp_dir("magi-unknown-workspace-projection");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, _, _) =
            accepted_session_store("unknown-workspace-session", Some("workspace-unknown"), 58);
        let session_id = SessionId::new("unknown-workspace-session");
        install_test_event_authority(&repository, &session_store);

        let error = repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect_err("unknown workspace must not persist as personal session");
        assert!(error.to_string().contains("未注册 workspace"));
        assert!(
            !repository
                .session_projection_root()
                .join(StateRepository::session_projection_file_name(&session_id))
                .exists()
        );

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
            .append_session_transaction(
                &session_id,
                &[CanonicalTurnMutation {
                    previous: None,
                    next: acceptance.canonical_turn.clone(),
                }],
                &[],
                Some((&acceptance, Some(&task))),
            )
            .expect("accepted event transaction should commit");
        // accepted 事务之后继续追加 canonical item 并完成 Turn，模拟 projection 尚未
        // checkpoint 时 daemon 退出。恢复必须从 event-only canonical 结果重建旧 sidecar。
        session_store.install_session_event_writer(Arc::new(repository.clone()));
        magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
            .append_item_sidecar(
                &session_id,
                Some(turn_id.as_str()),
                ActiveExecutionTurnItem {
                    item_id: "event-only-recovered-item".to_string(),
                    item_seq: 1,
                    kind: "assistant_stream".to_string(),
                    status: "running".to_string(),
                    source: "orchestrator".to_string(),
                    title: None,
                    content: Some("event-only canonical item".to_string()),
                    task_id: None,
                    worker_id: None,
                    role_id: None,
                    tool_call_id: None,
                    tool_name: None,
                    tool_status: None,
                    tool_arguments: None,
                    tool_result: None,
                    tool_error: None,
                    request_id: None,
                    user_message_id: None,
                    placeholder_message_id: None,
                    metadata: HashMap::new(),
                    timeline_entry_id: None,
                    source_thread_id: ThreadId::new(format!("thread-orchestrator-{session_id}")),
                },
            )
            .expect("event-only canonical item should append");
        magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
            .set_status_domain(&session_id, Some(turn_id.as_str()), "completed")
            .expect("event-only canonical turn should complete");
        assert!(
            !repository
                .session_projection_root()
                .join(StateRepository::session_projection_file_name(&session_id))
                .exists()
        );
        let restored_repository = StateRepository::new(state_root.clone());
        let (durable, sidecars) = restored_repository
            .load_session_projections(&[])
            .expect("event-only accepted state should recover");
        assert_eq!(durable.sessions.len(), 1);
        assert_eq!(durable.timeline.len(), 1);
        assert_eq!(durable.canonical_turns.len(), 1);
        assert_eq!(durable.canonical_turns[0].turn_id, turn_id);
        let recovered_sidecar = sidecars
            .runtime_sidecar(&session_id)
            .expect("event-only sidecar should be rebuilt");
        let recovered_turn = recovered_sidecar
            .current_turn
            .expect("event-only current turn should be rebuilt");
        assert_eq!(recovered_turn.status, "completed");
        assert_eq!(
            recovered_turn.items.len(),
            durable.canonical_turns[0].items.len()
        );
        let recovered_item = recovered_turn
            .items
            .iter()
            .find(|item| item.item_id == "event-only-recovered-item")
            .expect("event-only canonical item should be restored");
        assert_eq!(
            recovered_item.content.as_deref(),
            Some("event-only canonical item")
        );
        SessionStore::from_persisted_parts(durable.clone(), sidecars.clone())
            .expect("event-only canonical and rebuilt sidecar should pass strict recovery");
        restored_repository
            .validate_session_event_log_coverage(&durable)
            .expect("recovered acceptance should own its event log");

        let recovered_acceptance = restored_repository
            .load_accepted_submissions()
            .expect("accepted event should remain available until task checkpoint");
        assert_eq!(recovered_acceptance.len(), 1);
        assert!(recovered_acceptance[0].session_checkpointed);
        assert!(!recovered_acceptance[0].task_checkpointed);
        assert_eq!(
            recovered_acceptance[0]
                .task
                .as_ref()
                .expect("task acceptance should include task")
                .task_id,
            task_id
        );

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
    fn unowned_event_log_is_quarantined_instead_of_blocking_startup() {
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
        // 会话数据（例如其工作区被用户删除）已经不在时，事件日志不能阻止启动：
        // 整目录移到隔离区保留，不删除。
        let event_root = repository.session_event_root(&session_id);
        repository
            .validate_session_event_log_coverage(&SessionDurableState::default())
            .expect("unowned event log must be quarantined, not fail startup");
        assert!(!event_root.exists());
        let quarantined = state_root
            .join("orphaned-session-events")
            .join(event_root.file_name().expect("event root has a name"));
        assert!(quarantined.is_dir(), "event data must be preserved");

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn renaming_one_session_persists_only_that_session_and_survives_a_reload() {
        let state_root = unique_temp_dir("magi-rename-incremental");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, _, _) = accepted_session_store("rename-target", None, 61);
        let session_id = SessionId::new("rename-target");
        install_test_event_authority(&repository, &session_store);
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("initial projection should persist");

        // 与 daemon 装配里的 Rename 持久化回调相同：只提交被重命名的会话。
        let renamed = session_store
            .rename_session_with_persistence(
                &session_id,
                "咖啡店创意命名",
                |durable, sidecars| {
                    repository.save_session_projection_state_for_sessions(
                        durable,
                        sidecars,
                        std::slice::from_ref(&session_id),
                    )
                },
            )
            .expect("rename should persist");
        assert_eq!(renamed.title, "咖啡店创意命名");

        let reloaded = StateRepository::new(state_root.clone());
        let (restored, _) = reloaded
            .load_session_projections(&[])
            .expect("renamed projection must load");
        let title = restored
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)
            .map(|session| session.title.clone());
        assert_eq!(title.as_deref(), Some("咖啡店创意命名"));

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn restored_session_data_brings_its_quarantined_event_log_back() {
        let state_root = unique_temp_dir("magi-quarantine-restore");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, _, _) = accepted_session_store("quarantine-restore", None, 57);
        let session_id = SessionId::new("quarantine-restore");
        install_test_event_authority(&repository, &session_store);
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("session checkpoint should persist");
        let projection_path = repository
            .session_projection_root()
            .join(StateRepository::session_projection_file_name(&session_id));
        let parked = state_root.join("parked-projection.json");

        // 工作区被删除：投影消失，事件日志被隔离。
        fs::rename(&projection_path, &parked).expect("projection should be parked");
        repository
            .validate_session_event_log_coverage(&SessionDurableState::default())
            .expect("unowned event log is quarantined");
        let event_root = repository.session_event_root(&session_id);
        assert!(!event_root.exists());

        // 目录复原：投影回来，下一次加载前事件日志自动归位，会话完整恢复。
        fs::rename(&parked, &projection_path).expect("projection should be restored");
        let reloaded = StateRepository::new(state_root.clone());
        let (restored, _) = reloaded
            .load_session_projections(&[])
            .expect("restored projection must load with its event log");
        assert_eq!(restored.sessions.len(), 1);
        assert!(event_root.is_dir());
        assert!(
            !state_root
                .join("orphaned-session-events")
                .join(event_root.file_name().expect("event root has a name"))
                .exists()
        );
        reloaded
            .validate_session_event_log_coverage(&restored)
            .expect("restored session owns its event log");

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn model_step_journal_replays_after_checkpoint_and_layout_upgrade() {
        let state_root = unique_temp_dir("magi-step-journal");
        let repository = StateRepository::new(state_root.clone());
        fs::write(state_root.join("state-layout.json"), r#"{"version":4}"#).unwrap();
        repository.verify_state_layout(&[]).unwrap();
        let marker: StateLayoutMarker = repository
            .read_json_strict(&state_root.join("state-layout.json"))
            .unwrap();
        assert_eq!(marker.version, STATE_LAYOUT_VERSION);
        assert!(state_root.join("state-layout.v4.json").exists());
        let (store, turn_id, task_id) = accepted_session_store("step-journal", None, 10);
        let session = SessionId::new("step-journal");
        let (_, main_thread) = store.ensure_session_mission(&session, UtcMillis::now(), || {
            MissionId::new("mission-metered-steps")
        });
        store
            .create_goal(
                session.clone(),
                main_thread,
                turn_id.clone(),
                "metered steps",
                magi_core::AccessProfile::Restricted,
            )
            .unwrap();
        install_test_event_authority(&repository, &store);
        repository
            .save_session_projection_state(
                &store.durable_state(),
                &store.execution_sidecar_store_state(),
            )
            .unwrap();
        for index in 0..70 {
            let step_id = format!("model-call-{index}");
            let now = UtcMillis::now();
            store
                .begin_model_step(
                    &session,
                    &turn_id,
                    magi_session_store::ModelStep {
                        attempts: vec![magi_session_store::ModelAttempt {
                            attempt_id: format!("{step_id}:attempt:1"),
                            started_at: now,
                            settled_at: None,
                            status: magi_session_store::ModelStepStatus::Started,
                        }],
                        step_id: step_id.clone(),
                        task_id: Some(task_id.clone()),
                        thread_id: ThreadId::new("thread-step"),
                        goal_id: None,
                        usage_tokens: None,
                        goal_tokens_after: None,
                        checkpoint_generation: 0,
                        request_fingerprint: "a".repeat(64),
                        binding: None,
                        executor_capabilities: Some(
                            magi_bridge_client::native_executor_capabilities(),
                        ),
                        purpose: Some(magi_session_store::ModelStepPurpose::Execution),
                        provider: "test".into(),
                        message_count: 1,
                        tool_count: 0,
                        max_output_tokens: None,
                        input_request_ids: vec![],
                        started_at: now,
                        settled_at: None,
                        status: magi_session_store::ModelStepStatus::Started,
                        response_fingerprint: None,
                    },
                )
                .unwrap();
            if index == 0 {
                store
                    .start_model_step_retry(&session, &turn_id, &step_id)
                    .unwrap();
            }
            // The last request simulates a crash after send, before receiving a result.
            if index < 69 {
                store
                    .settle_model_step(
                        &session,
                        &turn_id,
                        &step_id,
                        Some("b".repeat(64)),
                        Some(2),
                        None,
                    )
                    .unwrap();
            }
        }
        for entry in fs::read_dir(repository.session_event_root(&session)).unwrap() {
            let path = entry.unwrap().path();
            if path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("checkpoint-")
            {
                continue;
            }
            let transaction: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            for event in transaction["events"].as_array().unwrap() {
                assert_eq!(event["kind"], "model_step_updated");
                assert!(
                    event.get("turn").is_none(),
                    "普通 Step 更新不能重写完整 Turn/Step 历史"
                );
                assert!(event.get("modelStep").is_some());
            }
        }
        // 模拟旧版 projection、checkpoint 和增量 Step 事件；读取迁移不能丢失 journal。
        fn add_retired_budget_fields(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(object) => {
                    if object.contains_key("stepId") && object.contains_key("requestFingerprint") {
                        object.insert("reservationTokens".into(), serde_json::json!(4096));
                    }
                    if object.contains_key("goalId") && object.contains_key("objective") {
                        object.insert("tokenBudget".into(), serde_json::json!(1));
                        object.insert("tokensReserved".into(), serde_json::json!(4096));
                        object.insert("budgetRequiredTokens".into(), serde_json::json!(4096));
                        object.insert("budgetActiveMillis".into(), serde_json::json!(1));
                        object.insert(
                            "runBudget".into(),
                            serde_json::json!({"maxModelAttempts":1}),
                        );
                        object.insert("status".into(), serde_json::json!("budget_limited"));
                    }
                    for child in object.values_mut() {
                        add_retired_budget_fields(child);
                    }
                }
                serde_json::Value::Array(values) => {
                    for child in values {
                        add_retired_budget_fields(child);
                    }
                }
                _ => {}
            }
        }
        for directory in [
            repository.session_projection_root(),
            repository.session_event_root(&session),
        ] {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().and_then(|value| value.to_str()) == Some("json") {
                    let mut value: serde_json::Value =
                        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                    add_retired_budget_fields(&mut value);
                    fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
                }
            }
        }
        let expected = store
            .canonical_turn_for_session_turn_id(&session, &turn_id)
            .unwrap();
        let (restored, _) = StateRepository::new(state_root.clone())
            .load_session_projections(&[])
            .unwrap();
        assert_eq!(
            restored.canonical_turns[0].model_steps,
            expected.model_steps
        );
        assert_eq!(
            restored.canonical_turns[0]
                .model_steps
                .last()
                .unwrap()
                .status,
            magi_session_store::ModelStepStatus::Started
        );
        assert_eq!(restored.canonical_turns[0].model_steps[0].attempts.len(), 2);
        let recovered =
            SessionStore::from_persisted_parts(restored, store.execution_sidecar_store_state())
                .unwrap();
        let goal = recovered.current_goal(&session).unwrap();
        assert_eq!(goal.status, magi_session_store::GoalStatus::Paused);
        let resumed = recovered
            .resume_goal_with_plan(&session, &goal.goal_id, goal.control_revision, None, None)
            .unwrap()
            .0;
        assert_eq!(resumed.tokens_used, goal.tokens_used);
        assert_eq!(
            goal.model_attempts_used, 71,
            "重放必须保留压缩/主子代理和重试的累计次数"
        );
        assert_eq!(
            goal.tokens_used, 138,
            "旧 projection 不得漏掉 checkpoint/事件中已结算的用量"
        );
        assert!(
            expected
                .model_steps
                .iter()
                .all(|step| step.goal_id.as_ref() == Some(&goal.goal_id))
        );

        assert!(
            fs::read_dir(repository.session_event_root(&session))
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("checkpoint-"))
        );
        // 模拟取消后才收到最后一个请求的响应；只追加结算事件，不重开 Turn。
        store
            .update_current_turn_status_for_turn_with_change(&session, Some(&turn_id), "cancelled")
            .unwrap();
        store
            .settle_model_step(
                &session,
                &turn_id,
                "model-call-69",
                Some("d".repeat(64)),
                Some(3),
                None,
            )
            .unwrap();
        let (late_durable, late_sidecars) = StateRepository::new(state_root.clone())
            .load_session_projections(&[])
            .unwrap();
        let late_restored =
            SessionStore::from_persisted_parts(late_durable, late_sidecars).unwrap();
        assert_eq!(
            late_restored.current_goal(&session).unwrap().tokens_used,
            141
        );
        let closed = late_restored
            .canonical_turn_for_session_turn_id(&session, &turn_id)
            .unwrap();
        assert_eq!(
            closed.status,
            magi_session_store::CanonicalTurnStatus::Cancelled
        );
        assert_eq!(
            closed.model_steps.last().unwrap().status,
            magi_session_store::ModelStepStatus::ResponseReceived
        );
        fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn user_question_answer_replays_from_event_log_with_a_stale_projection() {
        let state_root = unique_temp_dir("magi-question-journal");
        let repository = StateRepository::new(state_root.clone());
        repository.verify_state_layout(&[]).unwrap();
        let (store, turn_id, task_id) = accepted_session_store("question-journal", None, 10);
        let session = SessionId::new("question-journal");
        install_test_event_authority(&repository, &store);
        repository
            .save_session_projection_state(
                &store.durable_state(),
                &store.execution_sidecar_store_state(),
            )
            .unwrap();
        store.append_current_turn_item_for_turn(&session,Some(&turn_id),serde_json::from_value(serde_json::json!({
            "itemId":"question-journal-item", "itemSeq":1, "kind":"tool_call_started", "status":"running",
            "source":"orchestrator", "sourceThreadId":"thread-question-journal", "taskId":task_id,
            "toolName":"ask_user_question", "toolCallId":"question-journal-call"
        })).unwrap()).unwrap();
        let request = magi_session_store::user_question::PendingUserQuestion {
            question_id: "question-journal-id".into(),
            session_id: session.clone(),
            task_id: Some(task_id),
            turn_id: turn_id.clone(),
            tool_call_id: "question-journal-call".into(),
            requested_at: UtcMillis(11),
            can_skip: true,
            questions: magi_session_store::user_question::parse_questions(
                &serde_json::json!({"questions":[{
                    "header":"方案","question":"选哪个？","options":[{"label":"A"},{"label":"B"}]
                }]})
                .to_string(),
            )
            .unwrap(),
        };
        store.register_user_question(request.clone()).unwrap();
        // 没有刷新 projection，进程在等待时退出，新的 repository 只能沿日志恢复。
        let restored_repository = StateRepository::new(state_root.clone());
        let (durable, sidecars) = restored_repository.load_session_projections(&[]).unwrap();
        let restored = SessionStore::from_persisted_parts(durable, sidecars).unwrap();
        restored.install_session_event_writer(Arc::new(restored_repository.clone()));
        restored
            .interrupt_current_turn_by_daemon_restart(&session)
            .unwrap();
        assert_eq!(
            restored.pending_user_questions(&session).unwrap(),
            vec![request.clone()]
        );
        let response = magi_session_store::user_question::UserQuestionResponse::Answered {
            answers: vec![magi_session_store::user_question::UserQuestionAnswer {
                selected: vec!["B".into()],
                other: None,
            }],
        };
        restored
            .resolve_user_question(&session, &request.question_id, response.clone())
            .unwrap();
        let (durable, sidecars) = StateRepository::new(state_root.clone())
            .load_session_projections(&[])
            .unwrap();
        let reloaded = SessionStore::from_persisted_parts(durable, sidecars).unwrap();
        assert!(
            reloaded
                .pending_user_questions(&session)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reloaded
                .user_question_state(&session, &request.question_id)
                .unwrap()
                .unwrap()
                .response,
            Some(response)
        );
        fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn approval_decision_and_cross_turn_claim_replay_without_relaunching_a_tool() {
        use magi_session_store::tool_approval::{PendingToolApproval, ToolApprovalDecision};
        let state_root = unique_temp_dir("magi-approval-journal");
        let repository = StateRepository::new(state_root.clone());
        repository.verify_state_layout(&[]).unwrap();
        let (store, turn_id, task_id) = accepted_session_store("approval-journal", None, 10);
        let session = SessionId::new("approval-journal");
        install_test_event_authority(&repository, &store);
        repository
            .save_session_projection_state(
                &store.durable_state(),
                &store.execution_sidecar_store_state(),
            )
            .unwrap();
        let tool = |id: &str, call: &str| {
            serde_json::from_value(serde_json::json!({
            "itemId":id,"itemSeq":1,"kind":"tool_call_started","status":"running","source":"orchestrator", "sourceThreadId":"thread-approval-journal",
            "taskId":task_id,"toolName":"shell_exec","toolCallId":call,"toolArguments":"{\"command\":\"publish\"}"
        })).unwrap()
        };
        store
            .append_current_turn_item_for_turn(
                &session,
                Some(&turn_id),
                tool("approval-origin-item", "approval-origin-call"),
            )
            .unwrap();
        let request = PendingToolApproval {
            approval_id: "approval-journal-id".into(),
            session_id: session.clone(),
            task_id: task_id.clone(),
            turn_id: turn_id.clone(),
            tool_call_id: "approval-origin-call".into(),
            tool_name: "shell_exec".into(),
            reason: "publish".into(),
            requested_at: UtcMillis::now(),
            agent: None,
        };
        store
            .register_tool_approval(request.clone(), None, serde_json::Value::Null, None)
            .unwrap();
        let restored_repository = StateRepository::new(state_root.clone());
        let (durable, sidecars) = restored_repository.load_session_projections(&[]).unwrap();
        let restored = SessionStore::from_persisted_parts(durable, sidecars).unwrap();
        restored.install_session_event_writer(Arc::new(restored_repository));
        restored
            .interrupt_current_turn_by_daemon_restart(&session)
            .unwrap();
        assert_eq!(
            restored.pending_tool_approvals(&session).unwrap(),
            vec![request.clone()]
        );
        restored
            .resolve_tool_approvals(
                &session,
                std::slice::from_ref(&request.approval_id),
                ToolApprovalDecision::AllowOnce,
            )
            .unwrap();
        let now = UtcMillis::now();
        restored
            .accept_conversation_turn_with_timeline_entry(
                session.clone(),
                None,
                magi_session_store::TimelineEntryInput::new(
                    "approval-resume-input",
                    TimelineEntryKind::UserMessage,
                    "继续",
                    now,
                ),
                ActiveExecutionTurn {
                    turn_id: "approval-resume-turn".into(),
                    turn_seq: now.0,
                    accepted_at: now,
                    status: "running".into(),
                    completed_at: None,
                    user_message: Some("继续".into()),
                    items: vec![tool("approval-target-item", "approval-target-call")],
                },
            )
            .unwrap();
        let receipt = restored
            .claim_tool_approval(
                &session,
                &task_id,
                "approval-target-call",
                "shell_exec",
                r#"{"command":"publish"}"#,
                None,
                &serde_json::Value::Null,
                std::slice::from_ref(&task_id),
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(receipt.approval_id, request.approval_id);
        let (durable, sidecars) = StateRepository::new(state_root.clone())
            .load_session_projections(&[])
            .unwrap();
        let replayed = SessionStore::from_persisted_parts(durable, sidecars).unwrap();
        replayed
            .validate_approval_execution_links(&session)
            .unwrap();
        assert!(
            replayed
                .claim_tool_approval(
                    &session,
                    &task_id,
                    "approval-target-call",
                    "shell_exec",
                    r#"{"command":"publish"}"#,
                    None,
                    &serde_json::Value::Null,
                    std::slice::from_ref(&task_id),
                    None
                )
                .unwrap()
                .is_none()
        );
        let target = replayed
            .canonical_turn_for_session_turn_id(&session, "approval-resume-turn")
            .unwrap();
        assert_eq!(
            target.items[0]
                .tool
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap()["status"],
            "running"
        );
        fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn prior_state_layouts_upgrade_write_barrier_once() {
        for version in 2..=17 {
            let state_root = unique_temp_dir("magi-state-layout-upgrade");
            let repository = StateRepository::new(state_root.clone());
            fs::write(
                state_root.join("state-layout.json"),
                format!("{{\"version\":{version}}}"),
            )
            .unwrap();
            repository.verify_state_layout(&[]).unwrap();
            repository.verify_state_layout(&[]).unwrap();
            let old: StateLayoutMarker = repository
                .read_json_strict(&state_root.join(format!("state-layout.v{version}.json")))
                .unwrap();
            let current: StateLayoutMarker = repository
                .read_json_strict(&state_root.join("state-layout.json"))
                .unwrap();
            assert_eq!(old.version, version);
            assert_eq!(current.version, STATE_LAYOUT_VERSION);
            fs::remove_dir_all(state_root).unwrap();
        }
    }

    #[test]
    fn state_layout_only_accepts_the_current_marked_layout() {
        let fresh_root = unique_temp_dir("magi-layout-fresh");
        let fresh = StateRepository::new(fresh_root.clone());
        fresh
            .verify_state_layout(&[])
            .expect("fresh state root should accept the current layout");
        assert!(fresh_root.join("state-layout.json").exists());
        fresh
            .verify_state_layout(&[])
            .expect("marked current layout should verify again");

        let old_version_root = unique_temp_dir("magi-layout-old-version");
        fs::write(
            old_version_root.join("state-layout.json"),
            r#"{"version":1}"#,
        )
        .expect("old marker should write");
        StateRepository::new(old_version_root.clone())
            .verify_state_layout(&[])
            .expect_err("unsupported layout version must be rejected");

        for root in [fresh_root, old_version_root] {
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn committed_v2_layout_preserves_accepted_event_only_recovery() {
        let state_root = unique_temp_dir("magi-layout-accepted-event-recovery");
        let repository = StateRepository::new(state_root.clone());
        let (session_store, turn_id, task_id) =
            accepted_session_store("accepted-event-recovery", None, 60);
        let session_id = SessionId::new("accepted-event-recovery");
        let acceptance = session_store
            .session_acceptance_record(&session_id, &turn_id)
            .expect("accepted session should provide recovery record");
        repository
            .append_session_transaction(
                &session_id,
                &[CanonicalTurnMutation {
                    previous: None,
                    next: acceptance.canonical_turn.clone(),
                }],
                &[],
                Some((&acceptance, Some(&accepted_task(task_id, 60)))),
            )
            .expect("accepted event transaction should persist");
        repository
            .write_json_atomically(
                state_root.join("state-layout.json"),
                &StateLayoutMarker {
                    version: STATE_LAYOUT_VERSION,
                },
            )
            .expect("committed v2 marker should persist");

        repository
            .verify_state_layout(&[])
            .expect("accepted event-only crash window must remain recoverable");
        assert!(repository.session_event_root(&session_id).exists());
        let (restored, _) = StateRepository::new(state_root.clone())
            .load_session_projections(&[])
            .expect("accepted event should restore the missing session projection");
        assert_eq!(restored.sessions.len(), 1);
        assert_eq!(restored.sessions[0].session_id, session_id);

        let _ = fs::remove_dir_all(state_root);
    }

    fn resident_event_projections(repository: &StateRepository) -> usize {
        repository
            .session_event_cache
            .lock()
            .expect("event cache")
            .values()
            .filter(|projection| projection.is_resident())
            .count()
    }

    fn append_test_tool_item(
        session_store: &SessionStore,
        session_id: &SessionId,
        turn_id: &str,
        item_id: &str,
        item_seq: usize,
    ) {
        session_store
            .upsert_current_turn_item_for_turn(
                session_id,
                Some(turn_id),
                magi_session_store::ActiveExecutionTurnItem {
                    item_id: item_id.to_string(),
                    item_seq,
                    kind: "tool_call_result".to_string(),
                    status: "completed".to_string(),
                    source: "worker".to_string(),
                    title: Some("工具".to_string()),
                    content: Some("工具卡片".to_string()),
                    task_id: None,
                    worker_id: None,
                    role_id: None,
                    tool_call_id: Some(format!("call-{item_id}")),
                    tool_name: Some("file_read".to_string()),
                    tool_status: Some("completed".to_string()),
                    tool_arguments: Some("{}".to_string()),
                    tool_result: Some("{\"ok\":true}".to_string()),
                    tool_error: None,
                    request_id: None,
                    user_message_id: None,
                    placeholder_message_id: None,
                    metadata: Default::default(),
                    timeline_entry_id: None,
                    source_thread_id: magi_core::ThreadId::new("thread-main-default"),
                },
            )
            .expect("tool item should append through the canonical event writer");
    }

    #[test]
    fn restored_event_projections_release_turns_and_reload_on_the_next_write() {
        let state_root = unique_temp_dir("magi-event-projection-release");
        let session_id = SessionId::new("event-projection-release");
        {
            let repository = StateRepository::new(state_root.clone());
            let (session_store, turn_id, _task_id) =
                accepted_session_store("event-projection-release", None, 10);
            install_test_event_authority(&repository, &session_store);
            append_test_tool_item(&session_store, &session_id, &turn_id, "item-before", 5);
            repository
                .save_session_projection_state(
                    &session_store.durable_state(),
                    &session_store.execution_sidecar_store_state(),
                )
                .expect("initial projection should save");
        }

        // 重启：所有会话的事件回放结果只保留游标，不常驻完整回合。
        let repository = StateRepository::new(state_root.clone());
        let (durable, sidecars) = repository
            .load_session_projections(&[])
            .expect("projections should restore");
        assert_eq!(resident_event_projections(&repository), 0);
        assert!(
            repository
                .canonical_event_next_sequence(&session_id)
                .expect("cursor is available without resident turns")
                > 1
        );
        let session_store =
            SessionStore::from_persisted_parts(durable, sidecars).expect("store should restore");
        session_store.install_session_event_writer(Arc::new(repository.clone()));
        let restored_turn = session_store.canonical_turns_for_session(&session_id);
        let turn_id = restored_turn[0].turn_id.clone();

        // 完整保存直接使用 SessionStore 的回合，不为冷会话重放事件目录。
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("full save with released projections");
        assert_eq!(resident_event_projections(&repository), 0);

        // 下一次写入才从事件目录重放常驻，并且事实不丢。
        append_test_tool_item(&session_store, &session_id, &turn_id, "item-after", 6);
        assert_eq!(resident_event_projections(&repository), 1);
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("save after resident append");

        let reloaded = StateRepository::new(state_root.clone());
        let (durable, _) = reloaded
            .load_session_projections(&[])
            .expect("reload after append");
        let item_ids = durable
            .canonical_turns
            .iter()
            .flat_map(|turn| turn.items.iter().map(|item| item.item_id.as_str()))
            .collect::<Vec<_>>();
        assert!(item_ids.contains(&"item-before"), "{item_ids:?}");
        assert!(item_ids.contains(&"item-after"), "{item_ids:?}");
        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn resident_event_projections_are_capped_to_the_most_recently_used() {
        let mut cache = HashMap::new();
        for index in 0..(MAX_RESIDENT_EVENT_PROJECTIONS + 4) {
            let session_id = SessionId::new(format!("resident-cap-{index}"));
            let mut projection = SessionConversationProjection::default();
            projection.touch();
            cache.insert(session_id, projection);
        }
        trim_resident_event_projections(&mut cache);
        let resident = cache
            .iter()
            .filter(|(_, projection)| projection.is_resident())
            .map(|(session_id, _)| session_id.as_str().to_string())
            .collect::<HashSet<_>>();
        assert_eq!(resident.len(), MAX_RESIDENT_EVENT_PROJECTIONS);
        // 释放的是最早使用的那几个。
        for index in 4..(MAX_RESIDENT_EVENT_PROJECTIONS + 4) {
            assert!(resident.contains(&format!("resident-cap-{index}")));
        }
    }

    #[test]
    fn content_digest_distinguishes_content_and_length() {
        assert_eq!(
            ContentDigest::of("同一份内容"),
            ContentDigest::of("同一份内容")
        );
        assert_ne!(ContentDigest::of("a"), ContentDigest::of("b"));
        assert_ne!(ContentDigest::of("a"), ContentDigest::of("a "));
    }
}
