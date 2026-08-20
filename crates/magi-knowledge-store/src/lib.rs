pub mod code_scanner;
pub mod code_tokenizer;
pub mod dependency_graph;
mod governed_output;
pub mod graph;
mod graph_query;
pub mod index_persistence;
mod indexer;
pub mod inverted_index;
pub mod local_search_engine;
pub mod min_heap;
mod normalization;
mod query;
pub mod query_expander;
pub mod result_ranker;
pub mod search_cache;
mod source_model;
mod state;
pub mod symbol_index;
mod ts_symbol_extract;

#[cfg(test)]
mod tests;

use magi_core::{DomainError, UtcMillis, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use local_search_engine::{LocalSearchEngine, SearchEngineConfig, SearchOptions, SearchResult};
pub use local_search_engine::{SearchEngineStats, WorkspaceCodeIndexHealth};

pub use graph::{
    GraphDirection, GraphEdge, GraphEdgeKind, GraphEdgeOrigin, GraphEdgeStatus, GraphNode,
    GraphNodeKind, GraphNodeRef, GraphQuery, GraphStats, KnowledgeGraph, KnowledgeRelation,
};
use normalization::{normalize_code_index_ingestion, normalize_record};
pub use source_model::{
    CodeIndexIngestion, CodeIndexSource, CodeIndexSymbol, CodeSymbolKind, KnowledgeAuditLink,
    KnowledgeGovernanceLink, KnowledgeGovernanceOutcome,
};
pub use state::KnowledgeState;

const PROJECT_CODE_INDEX_ID: &str = "project-code-index";
const MAX_INFERRED_CANDIDATES_PER_WORKSPACE: usize = 2_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceIndexEnsureResult {
    Ready,
    Failed {
        reason_code: Option<code_scanner::CodeIndexScanReasonCode>,
    },
    TimedOut,
}

pub fn business_text_similarity(left: &str, right: &str) -> f32 {
    let left_terms = normalization::tokenize_business_text(left)
        .into_iter()
        .filter(|term| term.chars().count() >= 2)
        .collect::<HashSet<_>>();
    let right_terms = normalization::tokenize_business_text(right)
        .into_iter()
        .filter(|term| term.chars().count() >= 2)
        .collect::<HashSet<_>>();
    let min_size = left_terms.len().min(right_terms.len());
    if min_size == 0 {
        return 0.0;
    }
    let overlap = left_terms.intersection(&right_terms).count();
    overlap as f32 / min_size as f32
}

fn zero_utc_millis() -> UtcMillis {
    UtcMillis(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnowledgeKind {
    Adr,
    Faq,
    Learning,
    CodeIndex,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeRecord {
    pub knowledge_id: String,
    pub kind: KnowledgeKind,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    pub source_ref: Option<String>,
    #[serde(default = "zero_utc_millis")]
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeQuery {
    pub kind: Option<KnowledgeKind>,
    pub text: Option<String>,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeMatch {
    pub record: KnowledgeRecord,
    pub score: usize,
    pub matched_terms: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeQueryResult {
    pub records: Vec<KnowledgeRecord>,
    pub matches: Vec<KnowledgeMatch>,
    pub total_matches: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernedKnowledgeOutput {
    pub knowledge_id: String,
    pub title: String,
    pub kind: KnowledgeKind,
    pub excerpt: String,
    pub updated_at: UtcMillis,
    pub score: usize,
    pub matched_terms: Vec<String>,
    pub source_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_source: Option<CodeIndexSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_link: Option<KnowledgeAuditLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub governance_link: Option<KnowledgeGovernanceLink>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernedKnowledgeQueryResult {
    pub results: Vec<GovernedKnowledgeOutput>,
    pub total_matches: usize,
    pub truncated: bool,
}

/// 工作区代码检索引擎句柄：每个 workspace 一个 LocalSearchEngine。
///
/// 检索只读取不可变索引并使用内部线程安全缓存，因此同一 workspace 可并发查询；
/// 文件监听和定期对账通过写锁独占更新索引。
type WorkspaceSearchEngines = Arc<RwLock<HashMap<WorkspaceId, Arc<RwLock<LocalSearchEngine>>>>>;

/// 每个 workspace 的文件监听句柄：持有它仅为维持监听任务存活。
/// 与索引引擎同生命周期——build_workspace_index 时一并建立，store 释放时随之 drop。
type WorkspaceWatchers = Arc<RwLock<HashMap<WorkspaceId, Arc<magi_snapshot::watcher::FsWatcher>>>>;

type IndexPersistenceCallback = Arc<dyn Fn(&KnowledgeStore) + Send + Sync>;

#[derive(Clone, Default)]
pub struct KnowledgeStore {
    state: Arc<RwLock<KnowledgeState>>,
    search_engines: WorkspaceSearchEngines,
    watchers: WorkspaceWatchers,
    workspace_roots: Arc<RwLock<HashMap<WorkspaceId, std::path::PathBuf>>>,
    index_builds: Arc<Mutex<HashMap<WorkspaceId, bool>>>,
    index_outcomes: Arc<RwLock<HashMap<WorkspaceId, code_scanner::CodeIndexScanOutcome>>>,
    index_persist_callback: Arc<RwLock<Option<IndexPersistenceCallback>>>,
}

impl std::fmt::Debug for KnowledgeStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KnowledgeStore")
            .field("state", &self.state)
            .field(
                "search_engines",
                &self
                    .search_engines
                    .read()
                    .map(|engines| engines.len())
                    .unwrap_or(0),
            )
            .finish()
    }
}

#[derive(Clone, Debug, Default)]
pub struct KnowledgeIndexer;

#[derive(Clone, Debug, Default)]
pub struct KnowledgeQueryService;

#[derive(Clone, Debug, Default)]
pub struct GovernedKnowledgeService;

impl KnowledgeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册索引关系变更后的持久化回调。回调只在自动候选集合实际变化时触发，
    /// 由上层决定具体持久化介质；未注册时保持纯内存行为，便于 API 和单元测试复用。
    pub fn set_index_persistence_callback(
        &self,
        callback: impl Fn(&KnowledgeStore) + Send + Sync + 'static,
    ) {
        *self
            .index_persist_callback
            .write()
            .expect("knowledge store index persistence callback lock poisoned") =
            Some(Arc::new(callback));
    }

    fn notify_index_persistence(&self) {
        let callback = self
            .index_persist_callback
            .read()
            .expect("knowledge store index persistence callback lock poisoned")
            .clone();
        if let Some(callback) = callback {
            callback(self);
        }
    }

    pub fn from_state(mut state: KnowledgeState) -> Self {
        for record in state.entries.values_mut() {
            if record.created_at.0 == 0 {
                record.created_at = record.updated_at;
            }
        }
        state.rebuild_term_postings();
        Self {
            state: Arc::new(RwLock::new(state)),
            search_engines: Arc::default(),
            watchers: Arc::default(),
            workspace_roots: Arc::default(),
            index_builds: Arc::default(),
            index_outcomes: Arc::default(),
            index_persist_callback: Arc::default(),
        }
    }

    pub fn export_state(&self) -> KnowledgeState {
        self.state
            .read()
            .expect("knowledge store read lock poisoned")
            .clone()
    }

    pub fn index_persistence_callback_configured(&self) -> bool {
        self.index_persist_callback
            .read()
            .expect("knowledge store index persistence callback lock poisoned")
            .is_some()
    }

    /// 为指定 workspace 构建/重建本地代码检索索引。
    ///
    /// 复用 code_scanner 的扫描结果生成 (相对路径, 文件类型) 列表喂给
    /// LocalSearchEngine::build_index；文件内容由引擎内部按需读盘。
    pub fn build_workspace_index(
        &self,
        workspace_id: &WorkspaceId,
        workspace_root: &Path,
    ) -> code_scanner::CodeIndexScanOutcome {
        // 规范化 root：watcher（FSEvents 等）派发的事件路径是 OS canonical 形态
        // （macOS 上 /tmp → /private/tmp），引擎 to_relative 用 root 做前缀剥离。
        // 若两端 root 规范化来源不同，增量更新的相对路径会对不上，导致索引落空。
        // 在此统一 canonicalize，引擎与 watcher 共用同一规范化 root。
        let root =
            std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
        self.workspace_roots
            .write()
            .expect("knowledge store workspace roots write lock poisoned")
            .insert(workspace_id.clone(), root.clone());

        let outcome = code_scanner::scan_workspace(&root);
        let Some(summary) = outcome.summary.as_ref() else {
            let removed_code_index = self
                .delete(&workspace_project_code_index_id(workspace_id))
                .is_ok();
            self.clear_workspace_index_runtime(workspace_id);
            if outcome.status == code_scanner::CodeIndexScanStatus::Empty {
                index_persistence::IndexPersistence::new(&root.to_string_lossy()).invalidate();
                let mut engine =
                    LocalSearchEngine::new(&root.to_string_lossy(), SearchEngineConfig::default());
                engine.build_index(&[], UtcMillis::now().0);
                self.search_engines
                    .write()
                    .expect("knowledge store search engines write lock poisoned")
                    .insert(workspace_id.clone(), Arc::new(RwLock::new(engine)));
                self.spawn_watcher(workspace_id, &root);
                let relation_changed =
                    self.refresh_inferred_relations_for_workspace_inner(workspace_id) > 0;
                if removed_code_index || relation_changed {
                    self.notify_index_persistence();
                }
            }
            if removed_code_index && outcome.status != code_scanner::CodeIndexScanStatus::Empty {
                self.notify_index_persistence();
            }
            self.record_workspace_index_outcome(workspace_id, outcome.clone());
            return outcome;
        };
        let files: Vec<(String, String)> = summary
            .files
            .iter()
            .map(|f| (f.path.clone(), classify_index_file_type(&f.path)))
            .collect();

        let mut engine =
            LocalSearchEngine::new(&root.to_string_lossy(), SearchEngineConfig::default());
        engine.build_index_with_summary(&files, summary.clone());

        self.search_engines
            .write()
            .expect("knowledge store search engines write lock poisoned")
            .insert(workspace_id.clone(), Arc::new(RwLock::new(engine)));
        let code_index_changed = self.replace_code_index_projection(workspace_id, &root, summary);

        // 与索引构建原子地起文件监听：变更去抖后转发到增量更新。
        // 收敛 daemon 启动与 API 注册两条路径——所有 build 调用点自动获得 watcher。
        self.spawn_watcher(workspace_id, &root);
        let relation_changed =
            self.refresh_inferred_relations_for_workspace_inner(workspace_id) > 0;
        if code_index_changed || relation_changed {
            self.notify_index_persistence();
        }
        self.record_workspace_index_outcome(workspace_id, outcome.clone());
        outcome
    }

    pub fn begin_workspace_index_build(&self, workspace_id: &WorkspaceId) -> bool {
        let mut builds = self
            .index_builds
            .lock()
            .expect("knowledge store index builds lock poisoned");
        if builds.contains_key(workspace_id) {
            return false;
        }
        builds.insert(workspace_id.clone(), false);
        true
    }

    pub fn finish_workspace_index_build(&self, workspace_id: &WorkspaceId) -> bool {
        let discard_result = self
            .index_builds
            .lock()
            .expect("knowledge store index builds lock poisoned")
            .remove(workspace_id)
            .unwrap_or(false);
        if discard_result {
            self.delete_code_index_for_workspace(workspace_id);
        }
        discard_result
    }

    pub fn workspace_index_building(&self, workspace_id: &WorkspaceId) -> bool {
        self.index_builds
            .lock()
            .expect("knowledge store index builds lock poisoned")
            .contains_key(workspace_id)
    }

    pub fn workspace_index_build_cancelled(&self, workspace_id: &WorkspaceId) -> bool {
        self.index_builds
            .lock()
            .expect("knowledge store index builds lock poisoned")
            .get(workspace_id)
            .copied()
            .unwrap_or(false)
    }

    pub fn workspace_index_outcome(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Option<code_scanner::CodeIndexScanOutcome> {
        self.index_outcomes
            .read()
            .expect("knowledge store index outcomes read lock poisoned")
            .get(workspace_id)
            .cloned()
    }

    fn record_workspace_index_outcome(
        &self,
        workspace_id: &WorkspaceId,
        outcome: code_scanner::CodeIndexScanOutcome,
    ) {
        self.index_outcomes
            .write()
            .expect("knowledge store index outcomes write lock poisoned")
            .insert(workspace_id.clone(), outcome);
    }

    /// 为指定 workspace 起文件监听，把去抖后的变更转发到代码索引增量更新。
    ///
    /// 仅在 tokio 运行时存在时启动（FsWatcher 内部 spawn 去抖任务）；非 async
    /// 上下文（部分单测）直接跳过——监听只是增强，缺失不影响检索功能。
    /// 重复调用同一 workspace 会替换旧 watcher（旧句柄 drop 后监听停止）。
    fn spawn_watcher(&self, workspace_id: &WorkspaceId, workspace_root: &Path) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // 排除索引缓存与 git 目录，避免自激励循环（写 .magi/cache 又触发监听）。
        let excluded = Arc::new(vec![
            workspace_root.join(".magi"),
            workspace_root.join(".git"),
        ]);
        let watcher = match magi_snapshot::watcher::FsWatcher::start(workspace_root, excluded, tx) {
            Ok(watcher) => watcher,
            Err(_) => return,
        };

        let engines = self.search_engines.clone();
        let store = self.clone();
        let workspace_id_for_task = workspace_id.clone();
        tokio::spawn(async move {
            use magi_snapshot::watcher::DebouncedKind;
            let mut reconcile_interval = tokio::time::interval_at(
                tokio::time::Instant::now() + std::time::Duration::from_secs(30),
                std::time::Duration::from_secs(30),
            );
            reconcile_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    event = rx.recv() => {
                        let Some(event) = event else {
                            break;
                        };
                        let mut events = vec![(event.path.to_string_lossy().to_string(), event.kind)];
                        while let Ok(event) = rx.try_recv() {
                            events.push((event.path.to_string_lossy().to_string(), event.kind));
                        }
                        let engine = engines
                            .read()
                            .ok()
                            .and_then(|map| map.get(&workspace_id_for_task).cloned());
                        if let Some(engine) = engine {
                            let mut engine = engine.write().expect("search engine write lock poisoned");
                            let events: Vec<(String, local_search_engine::FileIndexEventKind)> = events
                                .into_iter()
                                .map(|(path, kind)| {
                                    let kind = match kind {
                                        DebouncedKind::Created => local_search_engine::FileIndexEventKind::Created,
                                        DebouncedKind::Modified => local_search_engine::FileIndexEventKind::Modified,
                                        DebouncedKind::Removed => local_search_engine::FileIndexEventKind::Deleted,
                                    };
                                    (path, kind)
                                })
                                .collect();
                            let has_deleted_event = events.iter().any(|(_, kind)| {
                                matches!(kind, local_search_engine::FileIndexEventKind::Deleted)
                            });
                            let has_missing_event_path = events.iter().any(|(path, _)| {
                                !std::path::Path::new(path).exists()
                            });
                            engine.apply_file_events(events);
                            // 删除事件在不同平台的路径形态并不完全一致；复用现有对账逻辑
                            // 立即核对索引文件集合，确保关联图能及时投影 dangling 状态。
                            if has_deleted_event || has_missing_event_path {
                                engine.reconcile_indexed_files();
                            }
                        }
                        let code_index_changed =
                            store.refresh_code_index_projection_from_runtime(&workspace_id_for_task);
                        let relation_changed = store
                            .refresh_inferred_relations_for_workspace_inner(&workspace_id_for_task)
                            > 0;
                        if code_index_changed || relation_changed {
                            store.notify_index_persistence();
                        }
                    }
                    _ = reconcile_interval.tick() => {
                        let engine = engines
                            .read()
                            .ok()
                            .and_then(|map| map.get(&workspace_id_for_task).cloned());
                        if let Some(engine) = engine {
                            engine
                                .write()
                                .expect("search engine write lock poisoned")
                                .reconcile_workspace_files();
                        }
                        let code_index_changed =
                            store.refresh_code_index_projection_from_runtime(&workspace_id_for_task);
                        let relation_changed = store
                            .refresh_inferred_relations_for_workspace_inner(&workspace_id_for_task)
                            > 0;
                        if code_index_changed || relation_changed {
                            store.notify_index_persistence();
                        }
                    }
                }
            }
        });

        self.watchers
            .write()
            .expect("knowledge store watchers write lock poisoned")
            .insert(workspace_id.clone(), Arc::new(watcher));
    }

    fn clear_workspace_index_runtime(&self, workspace_id: &WorkspaceId) {
        self.search_engines
            .write()
            .expect("knowledge store search engines write lock poisoned")
            .remove(workspace_id);
        self.watchers
            .write()
            .expect("knowledge store watchers write lock poisoned")
            .remove(workspace_id);
    }

    /// 在指定 workspace 的本地代码索引上检索；引擎未构建时返回 None。
    pub fn search_workspace_code(
        &self,
        workspace_id: &WorkspaceId,
        query: &str,
        options: SearchOptions,
    ) -> Option<Vec<SearchResult>> {
        let engine = self
            .search_engines
            .read()
            .expect("knowledge store search engines read lock poisoned")
            .get(workspace_id)
            .cloned()?;
        let engine = engine.read().expect("search engine read lock poisoned");
        Some(engine.search(query, options))
    }

    /// 按符号名查定义（goto_definition）。引擎未构建时返回 None。
    pub fn find_symbol_definitions(
        &self,
        workspace_id: &WorkspaceId,
        name: &str,
        max_results: usize,
    ) -> Option<Vec<symbol_index::SymbolEntry>> {
        let engine = self
            .search_engines
            .read()
            .expect("knowledge store search engines read lock poisoned")
            .get(workspace_id)
            .cloned()?;
        let engine = engine.read().expect("search engine read lock poisoned");
        Some(engine.find_symbol_definitions(name, max_results))
    }

    /// 列出某文件的全部符号（list_file_symbols）。引擎未构建时返回 None。
    pub fn list_file_symbols(
        &self,
        workspace_id: &WorkspaceId,
        file_path: &str,
    ) -> Option<Vec<symbol_index::SymbolEntry>> {
        let engine = self
            .search_engines
            .read()
            .expect("knowledge store search engines read lock poisoned")
            .get(workspace_id)
            .cloned()?;
        let engine = engine.read().expect("search engine read lock poisoned");
        Some(engine.list_file_symbols(file_path))
    }

    /// 指定 workspace 的检索引擎是否已就绪。
    pub fn workspace_index_ready(&self, workspace_id: &WorkspaceId) -> bool {
        self.search_engines
            .read()
            .expect("knowledge store search engines read lock poisoned")
            .get(workspace_id)
            .map(|engine| {
                let engine = engine.read().expect("search engine read lock poisoned");
                engine.is_ready() && engine.get_stats().total_documents > 0
            })
            .unwrap_or(false)
    }

    /// 保证指定工作区具有可查询的运行时索引。
    ///
    /// API 注册和 daemon 恢复可以继续异步构建；真正调用语义检索时，由工具入口通过
    /// 此方法等待正在进行的构建，或为尚未激活的恢复工作区按需完成一次构建。
    /// 空工作区也会保留一个可查询的空索引和 watcher，不再被误报为“引擎未就绪”。
    pub fn ensure_workspace_index_available(
        &self,
        workspace_id: &WorkspaceId,
        workspace_root: &Path,
        timeout: Duration,
    ) -> WorkspaceIndexEnsureResult {
        if self.workspace_index_available(workspace_id) {
            return WorkspaceIndexEnsureResult::Ready;
        }

        if self.begin_workspace_index_build(workspace_id) {
            let outcome = self.build_workspace_index(workspace_id, workspace_root);
            let cancelled = self.finish_workspace_index_build(workspace_id);
            if !cancelled && self.workspace_index_available(workspace_id) {
                return WorkspaceIndexEnsureResult::Ready;
            }
            return WorkspaceIndexEnsureResult::Failed {
                reason_code: outcome.reason_code,
            };
        }

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.workspace_index_available(workspace_id) {
                return WorkspaceIndexEnsureResult::Ready;
            }
            if !self.workspace_index_building(workspace_id) {
                return match self.workspace_index_outcome(workspace_id) {
                    Some(outcome)
                        if outcome.status == code_scanner::CodeIndexScanStatus::Failed =>
                    {
                        WorkspaceIndexEnsureResult::Failed {
                            reason_code: outcome.reason_code,
                        }
                    }
                    _ => WorkspaceIndexEnsureResult::Failed { reason_code: None },
                };
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        WorkspaceIndexEnsureResult::TimedOut
    }

    pub fn workspace_index_available(&self, workspace_id: &WorkspaceId) -> bool {
        self.search_engines
            .read()
            .expect("knowledge store search engines read lock poisoned")
            .get(workspace_id)
            .map(|engine| {
                engine
                    .read()
                    .expect("search engine read lock poisoned")
                    .is_ready()
            })
            .unwrap_or(false)
    }

    pub fn workspace_index_stats(&self, workspace_id: &WorkspaceId) -> Option<SearchEngineStats> {
        let engine = self
            .search_engines
            .read()
            .expect("knowledge store search engines read lock poisoned")
            .get(workspace_id)
            .cloned()?;
        let engine = engine.read().expect("search engine read lock poisoned");
        Some(engine.get_stats())
    }

    pub fn workspace_code_index_health(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Option<WorkspaceCodeIndexHealth> {
        let engine = self
            .search_engines
            .read()
            .expect("knowledge store search engines read lock poisoned")
            .get(workspace_id)
            .cloned()?;
        let engine = engine.read().expect("search engine read lock poisoned");
        Some(engine.code_index_health())
    }

    pub fn query_workspace_graph(
        &self,
        workspace_id: &WorkspaceId,
        query: &GraphQuery,
    ) -> Option<KnowledgeGraph> {
        let engine = self
            .search_engines
            .read()
            .expect("knowledge store search engines read lock poisoned")
            .get(workspace_id)
            .cloned()?;
        let code = engine
            .read()
            .expect("search engine read lock poisoned")
            .graph_snapshot();
        let knowledge = self
            .state
            .read()
            .expect("knowledge store read lock poisoned")
            .entries
            .values()
            .filter(|record| record.workspace_id.as_ref() == Some(workspace_id))
            .cloned()
            .collect();
        let relations = self
            .state
            .read()
            .expect("knowledge store read lock poisoned")
            .relations()
            .into_iter()
            .filter(|relation| relation.workspace_id == *workspace_id)
            .collect();
        Some(graph_query::build_workspace_graph(
            workspace_id,
            code,
            knowledge,
            relations,
            query,
        ))
    }

    /// 基于已完成的代码索引刷新知识到代码的自动候选关系。
    ///
    /// 该方法只生成可解释的候选，不会把推断结果直接升级为事实；已有的显式关系、
    /// 已确认关系和已忽略关系均保留。候选指纹由 workspace、source、kind、target
    /// 组成，因此重建索引不会产生重复关系。
    pub fn refresh_inferred_relations_for_workspace(&self, workspace_id: &WorkspaceId) -> usize {
        let refreshed = self.refresh_inferred_relations_for_workspace_inner(workspace_id);
        if refreshed > 0 {
            self.notify_index_persistence();
        }
        refreshed
    }

    fn refresh_inferred_relations_for_workspace_inner(&self, workspace_id: &WorkspaceId) -> usize {
        let Some(engine) = self
            .search_engines
            .read()
            .expect("knowledge store search engines read lock poisoned")
            .get(workspace_id)
            .cloned()
        else {
            return 0;
        };
        let code = engine
            .read()
            .expect("search engine read lock poisoned")
            .graph_snapshot();
        let records = self
            .state
            .read()
            .expect("knowledge store read lock poisoned")
            .entries
            .values()
            .filter(|record| {
                record.workspace_id.as_ref() == Some(workspace_id)
                    && record.kind != KnowledgeKind::CodeIndex
            })
            .cloned()
            .collect::<Vec<_>>();
        let candidates = infer_relation_candidates(workspace_id, &code, &records);
        let mut state = self
            .state
            .write()
            .expect("knowledge store write lock poisoned");
        let discovered_by_key = candidates
            .iter()
            .filter_map(|candidate| {
                candidate
                    .discovery_key
                    .clone()
                    .map(|key| (key, candidate.clone()))
            })
            .collect::<HashMap<_, _>>();
        let existing_by_key = state
            .relations
            .values()
            .filter(|relation| relation.workspace_id == *workspace_id)
            .filter_map(|relation| {
                relation_discovery_key(relation).map(|key| (key, relation.relation_id.clone()))
            })
            .collect::<HashMap<_, _>>();
        let explicit_relation_keys = state
            .relations
            .values()
            .filter(|relation| {
                relation.workspace_id == *workspace_id
                    && relation.origin != GraphEdgeOrigin::Inferred
            })
            .map(|relation| {
                relation_identity_key(&relation.source, relation.kind, &relation.target)
            })
            .collect::<HashSet<_>>();
        let mut refreshed = 0;

        // 未审阅候选必须跟随当前索引结果收敛：代码节点仍存在但已不再匹配时清除；
        // 目标节点已经消失时保留关系，交由图查询投影为 dangling，避免丢失失效线索。
        let stale_candidate_ids = state
            .relations
            .values()
            .filter(|relation| {
                relation.workspace_id == *workspace_id
                    && relation.origin == GraphEdgeOrigin::Inferred
                    && relation.status == GraphEdgeStatus::Candidate
                    && relation.reviewed_at.is_none()
            })
            .filter_map(|relation| {
                let key = relation_discovery_key(relation)?;
                if discovered_by_key.contains_key(&key)
                    || !code_graph_contains_target(&code, &relation.target)
                {
                    None
                } else {
                    Some(relation.relation_id.clone())
                }
            })
            .collect::<Vec<_>>();
        for relation_id in stale_candidate_ids {
            if state.delete_relation(&relation_id) {
                refreshed += 1;
            }
        }

        for mut candidate in candidates {
            let Some(discovery_key) = candidate.discovery_key.clone() else {
                continue;
            };
            if explicit_relation_keys.contains(&relation_identity_key(
                &candidate.source,
                candidate.kind,
                &candidate.target,
            )) {
                continue;
            }
            if let Some(existing_id) = existing_by_key.get(&discovery_key)
                && let Some(existing) = state.relations.get(existing_id).cloned()
            {
                if existing.status == GraphEdgeStatus::Rejected {
                    continue;
                }
                if existing.source != candidate.source
                    || existing.kind != candidate.kind
                    || existing.target != candidate.target
                {
                    // 用户已经修正过目标时，保留修正结果，不让下一次自动扫描覆盖它。
                    if existing.reviewed_at.is_some() {
                        continue;
                    }
                }
                candidate.relation_id = existing.relation_id.clone();
                candidate.created_at = existing.created_at;
                candidate.status = existing.status;
                candidate.reviewed_at = existing.reviewed_at;
                if relation_payload_equal(&existing, &candidate) {
                    continue;
                }
            }
            state.upsert_relation(candidate);
            refreshed += 1;
        }
        drop(state);
        refreshed
    }

    fn replace_code_index_projection(
        &self,
        workspace_id: &WorkspaceId,
        workspace_root: &Path,
        summary: &code_scanner::CodeIndexSummary,
    ) -> bool {
        let Some(ingestion) =
            code_scanner::code_index_ingestion_for_summary(workspace_root, summary)
        else {
            return false;
        };
        let code_index_id = workspace_project_code_index_id(workspace_id);
        let next_record = KnowledgeRecord {
            knowledge_id: code_index_id.clone(),
            kind: KnowledgeKind::CodeIndex,
            title: ingestion.title.clone(),
            content: ingestion.content.clone(),
            tags: ingestion.tags.clone(),
            workspace_id: Some(workspace_id.clone()),
            source_ref: ingestion.source_ref.clone(),
            created_at: ingestion.updated_at,
            updated_at: ingestion.updated_at,
        };
        let changed = self
            .state
            .read()
            .expect("knowledge store read lock poisoned")
            .get(&code_index_id)
            .map(|current| {
                current.title != next_record.title
                    || current.content != next_record.content
                    || current.tags != next_record.tags
                    || current.source_ref != next_record.source_ref
            })
            .unwrap_or(true);
        if changed {
            self.ingest_code_index_in_workspace(workspace_id.clone(), ingestion);
        }
        changed
    }

    fn refresh_code_index_projection_from_runtime(&self, workspace_id: &WorkspaceId) -> bool {
        let Some(workspace_root) = self
            .workspace_roots
            .read()
            .ok()
            .and_then(|roots| roots.get(workspace_id).cloned())
        else {
            return false;
        };
        let Some(summary) = self.runtime_code_index_summary_for_workspace(workspace_id) else {
            return false;
        };
        self.replace_code_index_projection(workspace_id, &workspace_root, &summary)
    }

    pub fn upsert(&self, record: KnowledgeRecord) {
        let normalized = normalize_record(record);
        let indexed_terms = KnowledgeIndexer::build_terms(&normalized);
        let workspace_id = normalized.workspace_id.clone();
        self.state
            .write()
            .expect("knowledge store write lock poisoned")
            .upsert(normalized, indexed_terms, None, None, None);
        if let Some(workspace_id) = workspace_id {
            self.refresh_inferred_relations_for_workspace_inner(&workspace_id);
        }
    }

    pub fn list_relations(&self, workspace_id: &WorkspaceId) -> Vec<KnowledgeRelation> {
        self.state
            .read()
            .expect("knowledge store read lock poisoned")
            .relations()
            .into_iter()
            .filter(|relation| relation.workspace_id == *workspace_id)
            .collect()
    }

    pub fn upsert_relation(&self, relation: KnowledgeRelation) -> Result<(), DomainError> {
        let relation = normalize_relation(relation)?;
        let mut state = self
            .state
            .write()
            .expect("knowledge store write lock poisoned");
        validate_relation(&state, &relation)?;
        state.upsert_relation(relation);
        Ok(())
    }

    pub fn replace_relation(
        &self,
        relation: KnowledgeRelation,
        workspace_id: &WorkspaceId,
    ) -> Result<(), DomainError> {
        if relation.workspace_id != *workspace_id {
            return Err(DomainError::InvalidState {
                message: format!(
                    "关系 {} 不属于 workspace {}",
                    relation.relation_id,
                    workspace_id.as_str()
                ),
            });
        }
        self.upsert_relation(relation)
    }

    pub fn relation(
        &self,
        relation_id: &str,
        workspace_id: &WorkspaceId,
    ) -> Option<KnowledgeRelation> {
        self.state
            .read()
            .expect("knowledge store read lock poisoned")
            .relations
            .get(relation_id)
            .filter(|relation| relation.workspace_id == *workspace_id)
            .cloned()
    }

    pub fn delete_relation_in_workspace(
        &self,
        relation_id: &str,
        workspace_id: &WorkspaceId,
    ) -> Result<(), DomainError> {
        let mut state = self
            .state
            .write()
            .expect("knowledge store write lock poisoned");
        let Some(relation) = state.relations.get(relation_id) else {
            return Err(DomainError::NotFound { entity: "relation" });
        };
        if relation.workspace_id != *workspace_id {
            return Err(DomainError::InvalidState {
                message: format!(
                    "关系 {relation_id} 不属于 workspace {}",
                    workspace_id.as_str()
                ),
            });
        }
        if !state.delete_relation(relation_id) {
            return Err(DomainError::NotFound { entity: "relation" });
        }
        Ok(())
    }

    pub fn ingest_code_index(&self, ingestion: CodeIndexIngestion) {
        self.ingest_code_index_with_workspace(ingestion, None);
    }

    pub fn ingest_code_index_in_workspace(
        &self,
        workspace_id: WorkspaceId,
        ingestion: CodeIndexIngestion,
    ) {
        self.ingest_code_index_with_workspace(ingestion, Some(workspace_id));
    }

    fn ingest_code_index_with_workspace(
        &self,
        ingestion: CodeIndexIngestion,
        workspace_id: Option<WorkspaceId>,
    ) {
        let mut normalized = normalize_code_index_ingestion(ingestion);
        if let Some(workspace_id) = workspace_id.as_ref()
            && normalized.knowledge_id == PROJECT_CODE_INDEX_ID
        {
            normalized.knowledge_id = workspace_project_code_index_id(workspace_id);
        }
        let record = KnowledgeRecord {
            knowledge_id: normalized.knowledge_id,
            kind: KnowledgeKind::CodeIndex,
            title: normalized.title,
            content: normalized.content,
            tags: normalized.tags,
            workspace_id,
            source_ref: normalized.source_ref,
            created_at: normalized.updated_at,
            updated_at: normalized.updated_at,
        };
        let indexed_terms = KnowledgeIndexer::build_terms_with_context(
            &record,
            Some(&normalized.source),
            normalized.audit.as_ref(),
            normalized.governance.as_ref(),
        );
        self.state
            .write()
            .expect("knowledge store write lock poisoned")
            .upsert(
                record,
                indexed_terms,
                Some(normalized.source),
                normalized.audit,
                normalized.governance,
            );
    }

    pub fn get(&self, knowledge_id: &str) -> Option<KnowledgeRecord> {
        self.state
            .read()
            .expect("knowledge store read lock poisoned")
            .get(knowledge_id)
    }

    pub fn list(&self) -> Vec<KnowledgeRecord> {
        self.state
            .read()
            .expect("knowledge store read lock poisoned")
            .list()
    }

    pub fn indexed_terms(&self, knowledge_id: &str) -> Vec<String> {
        self.state
            .read()
            .expect("knowledge store read lock poisoned")
            .indexed_terms(knowledge_id)
    }

    pub fn code_source(&self, knowledge_id: &str) -> Option<CodeIndexSource> {
        self.state
            .read()
            .expect("knowledge store read lock poisoned")
            .code_source(knowledge_id)
    }

    pub fn audit_link(&self, knowledge_id: &str) -> Option<KnowledgeAuditLink> {
        self.state
            .read()
            .expect("knowledge store read lock poisoned")
            .audit_link(knowledge_id)
    }

    pub fn governance_link(&self, knowledge_id: &str) -> Option<KnowledgeGovernanceLink> {
        self.state
            .read()
            .expect("knowledge store read lock poisoned")
            .governance_link(knowledge_id)
    }

    pub fn query(&self, query: &KnowledgeQuery) -> KnowledgeQueryResult {
        let state = self
            .state
            .read()
            .expect("knowledge store read lock poisoned");
        KnowledgeQueryService::execute(
            &state.entries,
            &state.index_terms,
            &state.term_postings,
            &state.code_sources,
            &state.audit_links,
            &state.governance_links,
            query,
        )
    }

    pub fn governed_output(&self, query: &KnowledgeQuery) -> Vec<GovernedKnowledgeOutput> {
        self.governed_query(query).results
    }

    pub fn governed_query(&self, query: &KnowledgeQuery) -> GovernedKnowledgeQueryResult {
        let state = self
            .state
            .read()
            .expect("knowledge store read lock poisoned");
        let query_result = KnowledgeQueryService::execute(
            &state.entries,
            &state.index_terms,
            &state.term_postings,
            &state.code_sources,
            &state.audit_links,
            &state.governance_links,
            query,
        );
        let total_matches = query_result.total_matches;
        let truncated = query_result.truncated;
        let results = GovernedKnowledgeService::project(
            query_result,
            &state.code_sources,
            &state.audit_links,
            &state.governance_links,
        );
        GovernedKnowledgeQueryResult {
            results,
            total_matches,
            truncated,
        }
    }

    pub fn code_index_summary_for_workspace(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Option<crate::code_scanner::CodeIndexSummary> {
        if let Some(summary) = self.runtime_code_index_summary_for_workspace(workspace_id) {
            return Some(summary);
        }

        let state = self
            .state
            .read()
            .expect("knowledge store read lock poisoned");

        state
            .entries
            .values()
            .filter(|record| {
                record.kind == KnowledgeKind::CodeIndex
                    && record.workspace_id.as_ref() == Some(workspace_id)
            })
            .filter_map(|record| {
                serde_json::from_str::<crate::code_scanner::CodeIndexSummary>(&record.content)
                    .ok()
                    .map(|summary| (record.updated_at, record.knowledge_id.clone(), summary))
            })
            .max_by(|left, right| left.0.0.cmp(&right.0.0).then_with(|| left.1.cmp(&right.1)))
            .map(|(_, _, summary)| summary)
    }

    fn runtime_code_index_summary_for_workspace(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Option<crate::code_scanner::CodeIndexSummary> {
        let engine = self
            .search_engines
            .read()
            .expect("knowledge store search engines read lock poisoned")
            .get(workspace_id)
            .cloned()?;
        let mut engine = engine.write().expect("search engine write lock poisoned");
        engine.is_ready().then(|| engine.code_index_summary())
    }

    pub fn delete(&self, knowledge_id: &str) -> Result<(), DomainError> {
        let mut state = self
            .state
            .write()
            .expect("knowledge store write lock poisoned");
        if !state.delete(knowledge_id) {
            return Err(DomainError::NotFound {
                entity: "knowledge",
            });
        }
        Ok(())
    }

    pub fn delete_in_workspace(
        &self,
        knowledge_id: &str,
        workspace_id: &WorkspaceId,
    ) -> Result<(), DomainError> {
        let record = self.get(knowledge_id).ok_or(DomainError::NotFound {
            entity: "knowledge",
        })?;
        if record.workspace_id.as_ref() != Some(workspace_id) {
            return Err(DomainError::InvalidState {
                message: format!(
                    "知识记录 {knowledge_id} 不属于 workspace {}",
                    workspace_id.as_str()
                ),
            });
        }
        self.delete(knowledge_id)
    }

    pub fn delete_code_index_for_workspace(&self, workspace_id: &WorkspaceId) {
        let changed = self
            .delete(&workspace_project_code_index_id(workspace_id))
            .is_ok();
        self.clear_workspace_index_runtime(workspace_id);
        self.index_outcomes
            .write()
            .expect("knowledge store index outcomes write lock poisoned")
            .remove(workspace_id);
        if changed {
            self.notify_index_persistence();
        }
    }

    pub fn clear(&self) {
        {
            let mut state = self
                .state
                .write()
                .expect("knowledge store write lock poisoned");
            state.entries.clear();
            state.index_terms.clear();
            state.term_postings.clear();
            state.code_sources.clear();
            state.audit_links.clear();
            state.governance_links.clear();
            state.relations.clear();
        }
        self.search_engines
            .write()
            .expect("knowledge store search engines write lock poisoned")
            .clear();
        self.watchers
            .write()
            .expect("knowledge store watchers write lock poisoned")
            .clear();
        self.workspace_roots
            .write()
            .expect("knowledge store workspace roots write lock poisoned")
            .clear();
        for cancelled in self
            .index_builds
            .lock()
            .expect("knowledge store index builds lock poisoned")
            .values_mut()
        {
            *cancelled = true;
        }
        self.index_outcomes
            .write()
            .expect("knowledge store index outcomes write lock poisoned")
            .clear();
    }

    pub fn clear_workspace(&self, workspace_id: &WorkspaceId) {
        let knowledge_ids = self
            .list()
            .into_iter()
            .filter(|record| record.workspace_id.as_ref() == Some(workspace_id))
            .map(|record| record.knowledge_id)
            .collect::<Vec<_>>();
        for knowledge_id in knowledge_ids {
            let _ = self.delete(&knowledge_id);
        }
        self.clear_workspace_index_runtime(workspace_id);
        if let Some(cancelled) = self
            .index_builds
            .lock()
            .expect("knowledge store index builds lock poisoned")
            .get_mut(workspace_id)
        {
            *cancelled = true;
        }
        self.index_outcomes
            .write()
            .expect("knowledge store index outcomes write lock poisoned")
            .remove(workspace_id);
    }
}

fn normalize_relation(mut relation: KnowledgeRelation) -> Result<KnowledgeRelation, DomainError> {
    relation.relation_id = relation.relation_id.trim().to_string();
    if relation.relation_id.is_empty() {
        return Err(DomainError::Validation {
            message: "关系 ID 不能为空".to_string(),
        });
    }
    relation.source = normalize_node_ref(relation.source);
    relation.target = normalize_node_ref(relation.target);
    relation.evidence = relation
        .evidence
        .into_iter()
        .map(|evidence| evidence.trim().to_string())
        .filter(|evidence| !evidence.is_empty())
        .collect();
    relation.discovery_evidence = relation.discovery_evidence.map(|evidence| {
        evidence
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect()
    });
    if let Some(confidence) = relation.confidence
        && (!confidence.is_finite() || !(0.0..=1.0).contains(&confidence))
    {
        return Err(DomainError::Validation {
            message: "关系置信度必须在 0 到 1 之间".to_string(),
        });
    }
    if relation.origin == GraphEdgeOrigin::Inferred {
        match relation.status {
            GraphEdgeStatus::Candidate if relation.reviewed_at.is_some() => {
                return Err(DomainError::Validation {
                    message: "未审阅的推断关系不能携带审阅时间".to_string(),
                });
            }
            GraphEdgeStatus::Active | GraphEdgeStatus::Rejected
                if relation.reviewed_at.is_none() =>
            {
                return Err(DomainError::Validation {
                    message: "推断关系确认或忽略后必须记录审阅时间".to_string(),
                });
            }
            _ => {}
        }
    }
    Ok(relation)
}

struct InferredRelationTarget<'a> {
    target: GraphNodeRef,
    terms: HashSet<String>,
    path: &'a str,
    symbol: Option<&'a symbol_index::SymbolEntry>,
}

fn infer_relation_candidates(
    workspace_id: &WorkspaceId,
    code: &graph::CodeGraphSnapshot,
    records: &[KnowledgeRecord],
) -> Vec<KnowledgeRelation> {
    // 代码目标集合与知识记录无关，只能按一次目标预计算。原实现把
    // relation_tokens 放在“知识 × 文件/符号”的内层循环中；在真实工作区里，
    // 同一批文件和符号会被每条知识重复分词，导致索引构建时间随知识量和符号量
    // 相乘增长，并让 watcher 的每次刷新都长时间占用 CPU。
    let mut targets = code
        .files
        .iter()
        .map(|path| InferredRelationTarget {
            target: GraphNodeRef::File { path: path.clone() },
            terms: relation_tokens(path),
            path,
            symbol: None,
        })
        .collect::<Vec<_>>();
    targets.extend(code.symbols.iter().map(|symbol| {
        let qualified_name = symbol
            .container
            .as_ref()
            .map(|container| format!("{container}::{}", symbol.name))
            .unwrap_or_else(|| symbol.name.clone());
        let target = GraphNodeRef::Symbol {
            path: symbol.file_path.clone(),
            qualified_name,
            symbol_kind: graph::symbol_kind_label(symbol.kind).to_string(),
        };
        let target_terms = relation_tokens(&format!(
            "{} {} {}",
            symbol.file_path,
            symbol.name,
            target.id()
        ));
        InferredRelationTarget {
            target,
            terms: target_terms,
            path: &symbol.file_path,
            symbol: Some(symbol),
        }
    }));

    // 先按目标 token 建倒排表。只有至少命中一个知识词的目标才需要计算完整
    // 的匹配和候选，避免对每条知识扫描全部文件和符号。
    let mut targets_by_term = HashMap::<String, Vec<usize>>::new();
    for (target_index, target) in targets.iter().enumerate() {
        for term in &target.terms {
            targets_by_term
                .entry(term.clone())
                .or_default()
                .push(target_index);
        }
    }

    let mut candidates = Vec::new();
    let mut sorted_records = records.to_vec();
    sorted_records.sort_by(|left, right| left.knowledge_id.cmp(&right.knowledge_id));

    for record in sorted_records {
        let terms = relation_knowledge_terms(&record);
        if terms.is_empty() {
            continue;
        }

        let mut target_indices = terms
            .keys()
            .filter_map(|term| targets_by_term.get(term))
            .flat_map(|indices| indices.iter().copied())
            .collect::<Vec<_>>();
        target_indices.sort_unstable();
        target_indices.dedup();

        for target_index in target_indices {
            let target = &targets[target_index];
            let matched = matching_relation_terms(&terms, &target.terms);
            if !relation_match_is_useful(&terms, &matched) {
                continue;
            }
            candidates.push(build_inferred_relation(
                workspace_id,
                &record,
                target.target.clone(),
                matched,
                target.path,
                target.symbol,
            ));
        }
    }

    candidates.sort_by(|left, right| {
        left.relation_id
            .cmp(&right.relation_id)
            .then_with(|| left.target.id().cmp(&right.target.id()))
    });
    candidates.dedup_by(|left, right| left.discovery_key == right.discovery_key);
    candidates.sort_by(|left, right| {
        right
            .confidence
            .unwrap_or_default()
            .total_cmp(&left.confidence.unwrap_or_default())
            .then_with(|| left.relation_id.cmp(&right.relation_id))
    });
    candidates.truncate(MAX_INFERRED_CANDIDATES_PER_WORKSPACE);
    candidates.sort_by(|left, right| left.relation_id.cmp(&right.relation_id));
    candidates
}

fn code_graph_contains_target(code: &graph::CodeGraphSnapshot, target: &GraphNodeRef) -> bool {
    match target {
        GraphNodeRef::File { path } => code.files.iter().any(|candidate| candidate == path),
        GraphNodeRef::Symbol {
            path,
            qualified_name,
            symbol_kind,
        } => code.symbols.iter().any(|symbol| {
            if symbol.file_path != *path || graph::symbol_kind_label(symbol.kind) != symbol_kind {
                return false;
            }
            let candidate_name = symbol
                .container
                .as_ref()
                .map(|container| format!("{container}::{}", symbol.name))
                .unwrap_or_else(|| symbol.name.clone());
            candidate_name == *qualified_name
        }),
        GraphNodeRef::Knowledge { .. } => false,
    }
}

fn relation_payload_equal(left: &KnowledgeRelation, right: &KnowledgeRelation) -> bool {
    left.workspace_id == right.workspace_id
        && left.source == right.source
        && left.kind == right.kind
        && left.target == right.target
        && left.origin == right.origin
        && left.confidence == right.confidence
        && left.status == right.status
        && left.evidence == right.evidence
        && left.discovery_key == right.discovery_key
        && left.discovery_evidence == right.discovery_evidence
        && left.reviewed_at == right.reviewed_at
}

fn relation_knowledge_terms(record: &KnowledgeRecord) -> HashMap<String, u8> {
    let mut terms = HashMap::new();
    add_relation_terms(&mut terms, &record.title, 3);
    for tag in &record.tags {
        add_relation_terms(&mut terms, tag, 2);
    }
    add_relation_terms(&mut terms, &record.content, 1);
    terms.retain(|term, _| !is_ignored_relation_token(term));
    terms
}

fn add_relation_terms(terms: &mut HashMap<String, u8>, value: &str, weight: u8) {
    for token in relation_tokens(value) {
        let entry = terms.entry(token).or_default();
        *entry = (*entry).max(weight);
    }
}

fn relation_tokens(value: &str) -> HashSet<String> {
    let mut tokens = normalization::tokenize_business_text(value)
        .into_iter()
        .collect::<HashSet<_>>();
    for part in value.split(|character: char| !character.is_alphanumeric()) {
        let mut current = String::new();
        let mut previous_is_lower = false;
        for character in part.chars() {
            if character.is_uppercase() && previous_is_lower && !current.is_empty() {
                tokens.insert(current.to_ascii_lowercase());
                current.clear();
            }
            if character.is_alphanumeric() {
                current.push(character);
                previous_is_lower = character.is_lowercase() || character.is_numeric();
            } else {
                if !current.is_empty() {
                    tokens.insert(current.to_ascii_lowercase());
                    current.clear();
                }
                previous_is_lower = false;
            }
        }
        if !current.is_empty() {
            tokens.insert(current.to_ascii_lowercase());
        }
    }
    tokens.retain(|token| !is_ignored_relation_token(token));
    tokens
}

fn is_ignored_relation_token(token: &str) -> bool {
    let character_count = token.chars().count();
    (token.is_ascii() && character_count < 3)
        || matches!(
            token,
            "src"
                | "test"
                | "tests"
                | "spec"
                | "main"
                | "lib"
                | "index"
                | "file"
                | "code"
                | "project"
                | "this"
                | "that"
                | "with"
                | "from"
                | "into"
                | "the"
                | "and"
                | "for"
        )
}

fn matching_relation_terms(
    knowledge_terms: &HashMap<String, u8>,
    target_terms: &HashSet<String>,
) -> Vec<String> {
    let mut matched = knowledge_terms
        .keys()
        .filter(|term| target_terms.contains(*term))
        .cloned()
        .collect::<Vec<_>>();
    matched.sort();
    matched
}

fn relation_match_is_useful(knowledge_terms: &HashMap<String, u8>, matched: &[String]) -> bool {
    if matched.is_empty() {
        return false;
    }
    let weighted_match = matched
        .iter()
        .map(|term| knowledge_terms.get(term).copied().unwrap_or(1))
        .sum::<u8>();
    weighted_match >= 2 || matched.len() >= 2
}

fn build_inferred_relation(
    workspace_id: &WorkspaceId,
    record: &KnowledgeRecord,
    target: GraphNodeRef,
    matched: Vec<String>,
    path: &str,
    symbol: Option<&symbol_index::SymbolEntry>,
) -> KnowledgeRelation {
    let kind = GraphEdgeKind::AppliesTo;
    let discovery_key = make_discovery_key(workspace_id, &record.knowledge_id, kind, &target);
    let relation_id = format!("inferred-{:016x}", stable_relation_hash(&discovery_key));
    let confidence = (0.55 + (matched.len().min(4) as f32 * 0.1)).min(0.95);
    let mut evidence = vec![format!("matched_tokens: {}", matched.join(", "))];
    evidence.push(format!("code_path: {path}"));
    if let Some(symbol) = symbol {
        evidence.push(format!(
            "symbol: {} ({}) at line {}",
            symbol.name,
            graph::symbol_kind_label(symbol.kind),
            symbol.line
        ));
    }
    let discovery_evidence = evidence.clone();
    let now = UtcMillis::now();
    KnowledgeRelation {
        relation_id,
        workspace_id: workspace_id.clone(),
        source: GraphNodeRef::Knowledge {
            knowledge_id: record.knowledge_id.clone(),
        },
        kind,
        target,
        origin: GraphEdgeOrigin::Inferred,
        confidence: Some(confidence),
        status: GraphEdgeStatus::Candidate,
        evidence,
        discovery_key: Some(discovery_key),
        discovery_evidence: Some(discovery_evidence),
        reviewed_at: None,
        created_at: now,
        updated_at: now,
    }
}

fn make_discovery_key(
    workspace_id: &WorkspaceId,
    source_id: &str,
    kind: GraphEdgeKind,
    target: &GraphNodeRef,
) -> String {
    format!(
        "{}|knowledge:{}|{}|{}",
        workspace_id.as_str(),
        source_id,
        graph_edge_kind_key(kind),
        target.id()
    )
}

fn relation_identity_key(
    source: &GraphNodeRef,
    kind: GraphEdgeKind,
    target: &GraphNodeRef,
) -> String {
    format!(
        "{}|{}|{}",
        source.id(),
        graph_edge_kind_key(kind),
        target.id()
    )
}

fn relation_discovery_key(relation: &KnowledgeRelation) -> Option<String> {
    relation.discovery_key.clone().or_else(|| {
        (relation.origin == GraphEdgeOrigin::Inferred).then(|| {
            let GraphNodeRef::Knowledge { knowledge_id } = &relation.source else {
                return String::new();
            };
            make_discovery_key(
                &relation.workspace_id,
                knowledge_id,
                relation.kind,
                &relation.target,
            )
        })
    })
}

fn graph_edge_kind_key(kind: GraphEdgeKind) -> &'static str {
    match kind {
        GraphEdgeKind::Contains => "contains",
        GraphEdgeKind::DependsOn => "depends_on",
        GraphEdgeKind::AppliesTo => "applies_to",
        GraphEdgeKind::Explains => "explains",
        GraphEdgeKind::References => "references",
        GraphEdgeKind::RelatedTo => "related_to",
        GraphEdgeKind::Supersedes => "supersedes",
        GraphEdgeKind::Contradicts => "contradicts",
    }
}

fn stable_relation_hash(value: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn normalize_node_ref(reference: GraphNodeRef) -> GraphNodeRef {
    match reference {
        GraphNodeRef::Knowledge { knowledge_id } => GraphNodeRef::Knowledge {
            knowledge_id: knowledge_id.trim().to_string(),
        },
        GraphNodeRef::File { path } => GraphNodeRef::File {
            path: path.trim().replace('\\', "/"),
        },
        GraphNodeRef::Symbol {
            path,
            qualified_name,
            symbol_kind,
        } => GraphNodeRef::Symbol {
            path: path.trim().replace('\\', "/"),
            qualified_name: qualified_name.trim().to_string(),
            symbol_kind: symbol_kind.trim().to_string(),
        },
    }
}

fn validate_relation(
    state: &KnowledgeState,
    relation: &KnowledgeRelation,
) -> Result<(), DomainError> {
    if relation.kind == GraphEdgeKind::Contains || relation.kind == GraphEdgeKind::DependsOn {
        return Err(DomainError::Validation {
            message: "代码派生关系不能通过知识关系接口写入".to_string(),
        });
    }
    let GraphNodeRef::Knowledge {
        knowledge_id: source_id,
    } = &relation.source
    else {
        return Err(DomainError::Validation {
            message: "关系 source 必须是知识节点".to_string(),
        });
    };
    validate_knowledge_ref(state, &relation.workspace_id, source_id, "source")?;
    validate_graph_node_ref(state, &relation.workspace_id, &relation.target, "target")?;
    if relation.source.id() == relation.target.id() {
        return Err(DomainError::Validation {
            message: "关系 source 和 target 不能相同".to_string(),
        });
    }
    Ok(())
}

fn validate_knowledge_ref(
    state: &KnowledgeState,
    workspace_id: &WorkspaceId,
    knowledge_id: &str,
    field: &str,
) -> Result<(), DomainError> {
    let Some(record) = state.entries.get(knowledge_id) else {
        return Err(DomainError::NotFound {
            entity: "knowledge",
        });
    };
    if record.workspace_id.as_ref() != Some(workspace_id) {
        return Err(DomainError::InvalidState {
            message: format!("{field} 知识节点不属于当前 workspace"),
        });
    }
    if record.kind == KnowledgeKind::CodeIndex {
        return Err(DomainError::Validation {
            message: format!("{field} 不能指向代码索引记录"),
        });
    }
    Ok(())
}

fn validate_graph_node_ref(
    state: &KnowledgeState,
    workspace_id: &WorkspaceId,
    reference: &GraphNodeRef,
    field: &str,
) -> Result<(), DomainError> {
    match reference {
        GraphNodeRef::Knowledge { knowledge_id } => {
            validate_knowledge_ref(state, workspace_id, knowledge_id, field)
        }
        GraphNodeRef::File { path } => validate_relative_graph_path(path, field),
        GraphNodeRef::Symbol {
            path,
            qualified_name,
            symbol_kind,
        } => {
            validate_relative_graph_path(path, field)?;
            if qualified_name.is_empty() || symbol_kind.is_empty() {
                return Err(DomainError::Validation {
                    message: format!("{field} 符号引用缺少 qualifiedName 或 symbolKind"),
                });
            }
            Ok(())
        }
    }
}

fn validate_relative_graph_path(path: &str, field: &str) -> Result<(), DomainError> {
    if path.is_empty()
        || path.contains('\\')
        || Path::new(path).is_absolute()
        || Path::new(path)
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err(DomainError::Validation {
            message: format!("{field} 文件路径必须是工作区内的相对路径"),
        });
    }
    Ok(())
}

fn workspace_project_code_index_id(workspace_id: &WorkspaceId) -> String {
    format!("{PROJECT_CODE_INDEX_ID}:{}", workspace_id.as_str())
}

/// 按文件路径粗分类型（source/test/config/doc），供 LocalSearchEngine::build_index 使用。
fn classify_index_file_type(file_path: &str) -> String {
    let lower = file_path.to_lowercase();
    let base = Path::new(&lower)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    if base.contains(".test.")
        || base.contains(".spec.")
        || lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
    {
        return "test".to_string();
    }

    let ext = Path::new(&lower)
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    match ext.as_str() {
        "json" | "yaml" | "yml" | "toml" | "ini" | "env" | "cfg" => "config",
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "go" | "java" | "rs" | "c" | "h"
        | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "cs" | "php" | "rb" | "swift" | "kt" | "kts"
        | "m" | "mm" | "vue" | "svelte" => "source",
        _ => "doc",
    }
    .to_string()
}
