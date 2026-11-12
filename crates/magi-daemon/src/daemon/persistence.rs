mod project_migration;

use super::config::DaemonError;
use super::session_event_log::SessionConversationProjection;
use magi_core::{DomainError, DomainResult, SessionId, Task};
use magi_event_bus::AuditUsageLedgerSnapshot;
use magi_knowledge_store::KnowledgeState;
use magi_orchestrator::task_store::{TaskStore, TaskStoreSnapshot};
use magi_session_store::{
    CanonicalTurnMutation, GOAL_DERIVED_TURN_METADATA_KEYS, SessionAcceptanceRecord,
    SessionDurableState, SessionEventWriter, SessionExecutionSidecarStoreState,
    SessionInputMutation, SessionRuntimeSidecarState, SessionStore, UnavailableSession,
};
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
    sidecar: Option<SessionRuntimeSidecarState>,
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

const STATE_LAYOUT_VERSION: u32 = 21;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StateLayoutMarker {
    version: u32,
}

mod session_projections;
mod state_layout;
mod projection_transactions;
mod durable_state;

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

    fn read_json_strict<T>(&self, path: &Path) -> Result<T, DaemonError>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        let content = fs::read_to_string(path)?;
        serde_json::from_str(&content).map_err(|error| {
            DaemonError::internal(format!("解析状态文件失败 {}: {error}", path.display()))
        })
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
    first_json_difference(left, right, "$").is_none()
}

/// 返回两份 JSON 第一处语义差异的路径与两侧摘要；没有差异时返回 `None`。
/// 校验失败必须指出具体字段，否则只能看到“不一致”而无法判断是哪份事实被改写。
fn first_json_difference(
    left: &serde_json::Value,
    right: &serde_json::Value,
    path: &str,
) -> Option<String> {
    fn brief(value: &serde_json::Value) -> String {
        let text = value.to_string();
        if text.chars().count() > 160 {
            format!("{}…", text.chars().take(160).collect::<String>())
        } else {
            text
        }
    }
    let differs = || {
        Some(format!(
            "{path}: 缓存={} 重放={}",
            brief(left),
            brief(right)
        ))
    };
    match (left, right) {
        (serde_json::Value::Null, serde_json::Value::Null) => None,
        (serde_json::Value::Bool(left_value), serde_json::Value::Bool(right_value)) => {
            (left_value != right_value).then(differs).flatten()
        }
        (serde_json::Value::String(left_value), serde_json::Value::String(right_value)) => {
            (left_value != right_value).then(differs).flatten()
        }
        (serde_json::Value::Number(left_value), serde_json::Value::Number(right_value)) => {
            let equal = left_value == right_value
                || match (left_value.as_f64(), right_value.as_f64()) {
                    (Some(left_value), Some(right_value)) => {
                        left_value.is_finite()
                            && right_value.is_finite()
                            && left_value == right_value
                    }
                    _ => false,
                };
            (!equal).then(differs).flatten()
        }
        (serde_json::Value::Array(left_items), serde_json::Value::Array(right_items)) => {
            if left_items.len() != right_items.len() {
                return Some(format!(
                    "{path}: 数组长度 缓存={} 重放={}",
                    left_items.len(),
                    right_items.len()
                ));
            }
            left_items
                .iter()
                .zip(right_items)
                .enumerate()
                .find_map(|(index, (left, right))| {
                    first_json_difference(left, right, &format!("{path}[{index}]"))
                })
        }
        (serde_json::Value::Object(left_fields), serde_json::Value::Object(right_fields)) => {
            if let Some(key) = left_fields
                .keys()
                .find(|key| !right_fields.contains_key(*key))
            {
                return Some(format!("{path}.{key}: 仅缓存存在"));
            }
            if let Some(key) = right_fields
                .keys()
                .find(|key| !left_fields.contains_key(*key))
            {
                return Some(format!("{path}.{key}: 仅重放存在"));
            }
            left_fields.iter().find_map(|(key, left)| {
                first_json_difference(left, &right_fields[key], &format!("{path}.{key}"))
            })
        }
        _ => differs(),
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
}

#[derive(Clone)]
pub(crate) struct RuntimeSidecarPersistence {
    state_repository: StateRepository,
    session_store: Arc<SessionStore>,
    workspace_store: Arc<WorkspaceStore>,
}

impl RuntimeSidecarPersistence {
    pub(crate) fn new(
        state_repository: StateRepository,
        session_store: Arc<SessionStore>,
        workspace_store: Arc<WorkspaceStore>,
    ) -> Self {
        Self {
            state_repository,
            session_store,
            workspace_store,
        }
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
mod tests;
