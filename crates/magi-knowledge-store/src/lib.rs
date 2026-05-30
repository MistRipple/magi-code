pub mod code_scanner;
pub mod code_tokenizer;
pub mod dependency_graph;
mod governed_output;
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
pub mod semantic_reranker;
mod source_model;
mod state;
pub mod symbol_index;
mod ts_symbol_extract;

#[cfg(test)]
mod tests;

use magi_core::{DomainError, UtcMillis, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use local_search_engine::{LocalSearchEngine, SearchEngineConfig, SearchOptions, SearchResult};

use normalization::{normalize_code_index_ingestion, normalize_record};
pub use source_model::{
    CodeIndexIngestion, CodeIndexSource, CodeIndexSymbol, CodeSymbolKind, KnowledgeAuditLink,
    KnowledgeGovernanceLink, KnowledgeGovernanceOutcome,
};
pub use state::KnowledgeState;

const PROJECT_CODE_INDEX_ID: &str = "project-code-index";

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
/// search() 需要 &mut（写查询缓存），故用 Mutex 包裹单个引擎，
/// 检索时只锁定目标 workspace 的引擎，不阻塞其他 workspace。
type WorkspaceSearchEngines = Arc<RwLock<HashMap<WorkspaceId, Arc<Mutex<LocalSearchEngine>>>>>;

/// 每个 workspace 的文件监听句柄：持有它仅为维持监听任务存活。
/// 与索引引擎同生命周期——build_workspace_index 时一并建立，store 释放时随之 drop。
type WorkspaceWatchers = Arc<RwLock<HashMap<WorkspaceId, Arc<magi_snapshot::watcher::FsWatcher>>>>;

#[derive(Clone, Default)]
pub struct KnowledgeStore {
    state: Arc<RwLock<KnowledgeState>>,
    search_engines: WorkspaceSearchEngines,
    watchers: WorkspaceWatchers,
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

    pub fn from_state(state: KnowledgeState) -> Self {
        Self {
            state: Arc::new(RwLock::new(state)),
            search_engines: Arc::default(),
            watchers: Arc::default(),
        }
    }

    pub fn export_state(&self) -> KnowledgeState {
        self.state
            .read()
            .expect("knowledge store read lock poisoned")
            .clone()
    }

    /// 为指定 workspace 构建/重建本地代码检索索引。
    ///
    /// 复用 code_scanner 的扫描结果生成 (相对路径, 文件类型) 列表喂给
    /// LocalSearchEngine::build_index；文件内容由引擎内部按需读盘。
    pub fn build_workspace_index(&self, workspace_id: &WorkspaceId, workspace_root: &Path) {
        // 规范化 root：watcher（FSEvents 等）派发的事件路径是 OS canonical 形态
        // （macOS 上 /tmp → /private/tmp），引擎 to_relative 用 root 做前缀剥离。
        // 若两端 root 规范化来源不同，增量更新的相对路径会对不上，导致索引落空。
        // 在此统一 canonicalize，引擎与 watcher 共用同一规范化 root。
        let root =
            std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());

        let outcome = code_scanner::scan_workspace(&root);
        let Some(summary) = outcome.summary.as_ref() else {
            return;
        };
        let files: Vec<(String, String)> = summary
            .files
            .iter()
            .map(|f| (f.path.clone(), classify_index_file_type(&f.path)))
            .collect();

        let mut engine =
            LocalSearchEngine::new(&root.to_string_lossy(), SearchEngineConfig::default());
        engine.build_index(&files);

        self.search_engines
            .write()
            .expect("knowledge store search engines write lock poisoned")
            .insert(workspace_id.clone(), Arc::new(Mutex::new(engine)));

        // 与索引构建原子地起文件监听：变更去抖后转发到增量更新。
        // 收敛 daemon 启动与 API 注册两条路径——所有 build 调用点自动获得 watcher。
        self.spawn_watcher(workspace_id, workspace_root);
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
        let workspace_id_for_task = workspace_id.clone();
        tokio::spawn(async move {
            use magi_snapshot::watcher::DebouncedKind;
            while let Some(event) = rx.recv().await {
                let path = event.path.to_string_lossy().to_string();
                let engine = engines
                    .read()
                    .ok()
                    .and_then(|map| map.get(&workspace_id_for_task).cloned());
                if let Some(engine) = engine {
                    let mut engine = engine.lock().expect("search engine mutex poisoned");
                    // 按事件类型路由到引擎对应的增量入口（新增/修改/删除语义不同）。
                    match event.kind {
                        DebouncedKind::Created => engine.on_file_created(&path),
                        DebouncedKind::Modified => engine.on_file_changed(&path),
                        DebouncedKind::Removed => engine.on_file_deleted(&path),
                    }
                }
            }
        });

        self.watchers
            .write()
            .expect("knowledge store watchers write lock poisoned")
            .insert(workspace_id.clone(), Arc::new(watcher));
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
        let mut engine = engine.lock().expect("search engine mutex poisoned");
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
        let engine = engine.lock().expect("search engine mutex poisoned");
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
        let engine = engine.lock().expect("search engine mutex poisoned");
        Some(engine.list_file_symbols(file_path))
    }

    /// 指定 workspace 的检索引擎是否已就绪。
    pub fn workspace_index_ready(&self, workspace_id: &WorkspaceId) -> bool {
        self.search_engines
            .read()
            .expect("knowledge store search engines read lock poisoned")
            .get(workspace_id)
            .map(|engine| {
                engine
                    .lock()
                    .expect("search engine mutex poisoned")
                    .is_ready()
            })
            .unwrap_or(false)
    }

    pub fn upsert(&self, record: KnowledgeRecord) {
        let normalized = normalize_record(record);
        let indexed_terms = KnowledgeIndexer::build_terms(&normalized);
        self.state
            .write()
            .expect("knowledge store write lock poisoned")
            .upsert(normalized, indexed_terms, None, None, None);
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

    /// 获取代码索引摘要（用于 API 返回前端所需格式）
    pub fn code_index_summary(&self) -> Option<crate::code_scanner::CodeIndexSummary> {
        let state = self
            .state
            .read()
            .expect("knowledge store read lock poisoned");

        let record = state.entries.values().find(|r| {
            r.kind == KnowledgeKind::CodeIndex
                && r.workspace_id.is_none()
                && r.knowledge_id == PROJECT_CODE_INDEX_ID
        })?;

        serde_json::from_str(&record.content).ok()
    }

    pub fn code_index_summary_for_workspace(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Option<crate::code_scanner::CodeIndexSummary> {
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

    pub fn delete(&self, knowledge_id: &str) -> Result<(), DomainError> {
        let mut state = self
            .state
            .write()
            .expect("knowledge store write lock poisoned");
        if state.entries.remove(knowledge_id).is_none() {
            return Err(DomainError::NotFound {
                entity: "knowledge",
            });
        }
        state.index_terms.remove(knowledge_id);
        state.code_sources.remove(knowledge_id);
        state.audit_links.remove(knowledge_id);
        state.governance_links.remove(knowledge_id);
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

    pub fn delete_project_code_index(&self) {
        let _ = self.delete(PROJECT_CODE_INDEX_ID);
    }

    pub fn delete_code_index_for_workspace(&self, workspace_id: &WorkspaceId) {
        let _ = self.delete(&workspace_project_code_index_id(workspace_id));
    }

    pub fn clear(&self) {
        let mut state = self
            .state
            .write()
            .expect("knowledge store write lock poisoned");
        state.entries.clear();
        state.index_terms.clear();
        state.code_sources.clear();
        state.audit_links.clear();
        state.governance_links.clear();
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
    }
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
