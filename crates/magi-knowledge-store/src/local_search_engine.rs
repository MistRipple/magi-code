use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::code_tokenizer::CodeTokenizer;
use crate::dependency_graph::DependencyGraph;
use crate::index_persistence::{IndexPersistence, PersistenceSnapshot};
use crate::inverted_index::{IndexSearchHit, InvertedIndex};
use crate::query_expander::{LlmExpandResult, QueryExpander};
use crate::result_ranker::{
    RankBoostSignals, RankWeights, RankedResult, ResultRanker, ScoreDimensions,
};
use crate::search_cache::SearchCache;
use crate::semantic_reranker::SemanticReranker;
use crate::symbol_index::{SymbolIndex, SymbolSearchHit};

const RECENT_EDIT_TTL_MS: u64 = 30 * 60 * 1000;
const RECENT_EDIT_MAX_FILES: usize = 200;
const INDEX_CACHE_STATUS_READY: &str = "ready";
const INDEX_CACHE_STATUS_DEGRADED: &str = "degraded";
const INDEX_CACHE_SAVE_FAILED_CODE: &str = "index_cache_save_failed";

#[derive(Clone, Debug, Default)]
pub struct SearchOptions {
    pub max_results: Option<usize>,
    pub max_context_length: Option<usize>,
    pub preferred_scopes: Vec<String>,
    pub prefer_recent_edits: bool,
    pub llm_expand_result: Option<LlmExpandResult>,
    pub llm_rerank_response: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodeSnippet {
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub matched_tokens: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    pub tfidf: f64,
    pub symbol_match: f64,
    pub position_weight: f64,
    pub centrality: f64,
    pub recency: f64,
    pub type_weight: f64,
    pub recent_edit_boost: f64,
    pub scope_boost: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub file_path: String,
    pub score: f64,
    pub snippets: Vec<CodeSnippet>,
    pub score_breakdown: ScoreBreakdown,
}

#[derive(Clone, Debug, Default)]
pub struct SearchEngineConfig {
    pub rank_weights: Option<RankWeights>,
}

#[derive(Clone, Debug)]
pub struct SearchEngineStats {
    pub is_ready: bool,
    pub total_documents: usize,
    pub unique_tokens: usize,
    pub unique_symbols: usize,
    pub total_dep_edges: usize,
    pub cache_hit_rate: f64,
    pub index_version: u64,
    pub index_cache_status: &'static str,
    pub index_cache_error_code: Option<&'static str>,
}

pub struct LocalSearchEngine {
    project_root: String,
    inverted_index: InvertedIndex,
    symbol_index: SymbolIndex,
    dependency_graph: DependencyGraph,
    result_ranker: ResultRanker,
    search_cache: SearchCache<Vec<SearchResult>>,
    query_expander: QueryExpander,
    semantic_reranker: SemanticReranker,
    tokenizer: CodeTokenizer,
    persistence: IndexPersistence,
    indexed_files: Vec<(String, String)>,
    is_ready: bool,
    tracked_file_states: HashMap<String, (u64, u64)>,
    index_version: u64,
    index_cache_status: &'static str,
    index_cache_error_code: Option<&'static str>,
    project_vocabulary_dirty: bool,
    recent_edited_files: HashMap<String, u64>,
}

impl LocalSearchEngine {
    pub fn new(project_root: &str, config: SearchEngineConfig) -> Self {
        Self {
            project_root: project_root.to_string(),
            inverted_index: InvertedIndex::new(),
            symbol_index: SymbolIndex::new(),
            dependency_graph: DependencyGraph::new(),
            result_ranker: ResultRanker::new(config.rank_weights),
            search_cache: SearchCache::with_defaults(),
            query_expander: QueryExpander::new(),
            semantic_reranker: SemanticReranker::new(),
            tokenizer: CodeTokenizer::new(),
            persistence: IndexPersistence::new(project_root),
            indexed_files: Vec::new(),
            is_ready: false,
            tracked_file_states: HashMap::new(),
            index_version: 0,
            index_cache_status: INDEX_CACHE_STATUS_READY,
            index_cache_error_code: None,
            project_vocabulary_dirty: true,
            recent_edited_files: HashMap::new(),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.is_ready
    }

    pub fn build_index(&mut self, files: &[(String, String)]) {
        self.indexed_files = files.to_vec();

        let restore_result = self.persistence.load();
        if let Some(snapshot) = restore_result {
            if !snapshot_project_root_matches(&self.project_root, &snapshot.project_root) {
                tracing::warn!(
                    current_project_root = %self.project_root,
                    snapshot_project_root = %snapshot.project_root,
                    "local search index cache ignored because project root mismatched"
                );
                self.persistence.invalidate();
            } else {
                let freshness = self.persistence.validate_freshness(
                    &self.project_root,
                    &snapshot,
                    &self.indexed_files,
                );

                let total_files = snapshot.file_manifest.len() + freshness.added.len();
                if !IndexPersistence::should_full_rebuild(&freshness, total_files) {
                    if self.restore_from_snapshot(&snapshot, &freshness) {
                        if let Some(ref ec) = snapshot.expansion_cache {
                            self.query_expander.import_cache(ec.clone());
                        }
                        self.rebuild_tracked_file_states();
                        self.refresh_project_vocabulary_if_needed();
                        self.bump_index_version();
                        self.is_ready = true;
                        self.save_index();
                        return;
                    }
                }
            }
        }

        self.full_build(files);
        self.is_ready = true;
        self.rebuild_tracked_file_states();
        self.refresh_project_vocabulary_if_needed();
        self.bump_index_version();
        self.save_index();
    }

    pub fn search(&mut self, query: &str, options: SearchOptions) -> Vec<SearchResult> {
        let max_results = options.max_results.unwrap_or(10);
        let max_context_length = options.max_context_length.unwrap_or(8000);
        let cache_key = search_cache_key(query, &options);

        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }

        self.reconcile_indexed_files();

        if let Some(cached) = self.search_cache.get(&cache_key) {
            return cached;
        }

        let query_intent = detect_query_intent(trimmed);
        let query_tokens = self.tokenizer.tokenize_query(trimmed);
        if query_tokens.is_empty() {
            return Vec::new();
        }

        let mut expanded = self.query_expander.expand_offline(trimmed, &query_tokens);

        let should_expand = query_intent == QueryIntent::Semantic && query_tokens.len() <= 3;
        if should_expand {
            if let Some(llm_result) = options.llm_expand_result {
                self.query_expander
                    .merge_llm_result(&mut expanded, llm_result);
            }
        }

        let search_tokens = &expanded.expanded_tokens;

        let index_hits = if self.inverted_index.is_ready() {
            self.inverted_index.search(search_tokens, max_results * 3)
        } else {
            Vec::new()
        };

        let symbol_hits = if self.symbol_index.is_ready() {
            self.symbol_index
                .search_multi(search_tokens, max_results * 2, Some(trimmed))
        } else {
            Vec::new()
        };

        let centrality_map: HashMap<String, f64> = if self.dependency_graph.is_ready() {
            let mut map = HashMap::new();
            for hit in &index_hits {
                map.entry(hit.file_path.clone())
                    .or_insert_with(|| self.dependency_graph.get_centrality(&hit.file_path));
            }
            for hit in &symbol_hits {
                map.entry(hit.symbol.file_path.clone())
                    .or_insert_with(|| self.dependency_graph.get_centrality(&hit.symbol.file_path));
            }
            map
        } else {
            HashMap::new()
        };

        let file_timestamps: HashMap<String, u64> = {
            let mut ts = HashMap::new();
            for hit in &index_hits {
                if let Some(mtime) = self.inverted_index.get_document_meta(&hit.file_path) {
                    ts.insert(hit.file_path.clone(), mtime);
                }
            }
            ts
        };

        let weight_overrides = match query_intent {
            QueryIntent::Symbol => Some(RankWeights {
                symbol_match: 0.50,
                tfidf: 0.20,
                position_weight: 0.12,
                centrality: 0.08,
                recency: 0.05,
                type_weight: 0.05,
            }),
            QueryIntent::Semantic => expanded.weight_hints.map(|wh| {
                let mut w = RankWeights::default();
                if let Some(v) = wh.symbol_match {
                    w.symbol_match = v;
                }
                if let Some(v) = wh.tfidf {
                    w.tfidf = v;
                }
                if let Some(v) = wh.position_weight {
                    w.position_weight = v;
                }
                if let Some(v) = wh.centrality {
                    w.centrality = v;
                }
                if let Some(v) = wh.recency {
                    w.recency = v;
                }
                if let Some(v) = wh.type_weight {
                    w.type_weight = v;
                }
                w
            }),
        };

        let boost_signals = RankBoostSignals {
            preferred_scopes: normalize_scope_hints(&options.preferred_scopes),
            recent_edited_files: if options.prefer_recent_edits {
                self.get_recent_edited_file_set()
            } else {
                HashSet::new()
            },
        };

        let ranked = self.result_ranker.rank(
            &index_hits,
            &symbol_hits,
            &centrality_map,
            max_results * 2,
            &file_timestamps,
            weight_overrides.as_ref(),
            Some(&boost_signals),
        );

        let expanded_ranked = self.expand_with_dependencies(ranked, max_results * 2);

        let reranked = self.semantic_reranker.rerank(
            trimmed,
            expanded_ranked,
            options.llm_rerank_response.as_deref(),
            Some(&self.symbol_index),
            15,
        );

        let results = self.assemble_results(
            &reranked,
            &index_hits,
            &symbol_hits,
            max_results,
            max_context_length,
        );

        let file_paths: Vec<String> = results.iter().map(|r| r.file_path.clone()).collect();
        self.search_cache
            .set(&cache_key, results.clone(), file_paths);

        results
    }

    pub fn build_expansion_prompt(&self, query: &str) -> String {
        self.query_expander.build_llm_prompt(query)
    }

    pub fn parse_expansion_response(&self, content: &str) -> LlmExpandResult {
        self.query_expander.parse_llm_response(content)
    }

    pub fn build_rerank_prompt(
        &self,
        query: &str,
        candidates: &[RankedResult],
        top_n: usize,
    ) -> Option<String> {
        if candidates.len() <= 2 {
            return None;
        }
        Some(SemanticReranker::build_prompt(
            query,
            &candidates[..top_n.min(candidates.len())],
            Some(&self.symbol_index),
        ))
    }

    pub fn on_file_changed(&mut self, file_path: &str) {
        if !self.is_ready {
            return;
        }
        let Some(relative) = self.to_workspace_relative(file_path) else {
            return;
        };
        if !crate::code_scanner::is_indexable_code_path(&relative) {
            self.remove_stale_indexed_file(&relative);
            return;
        }
        let file_type = self.ensure_indexed_file_record(&relative, None);
        self.apply_changed_file(&relative, &file_type);
        self.refresh_project_vocabulary_if_needed();
        self.bump_index_version();
        self.save_index();
    }

    /// 按符号名查定义（goto_definition 的底层）。返回匹配的符号条目。
    pub fn find_symbol_definitions(
        &self,
        name: &str,
        max_results: usize,
    ) -> Vec<crate::symbol_index::SymbolEntry> {
        self.symbol_index
            .search(name, max_results)
            .into_iter()
            .map(|hit| hit.symbol)
            .collect()
    }

    /// 列出某文件的全部符号（list_file_symbols 的底层）。
    pub fn list_file_symbols(&self, file_path: &str) -> Vec<crate::symbol_index::SymbolEntry> {
        self.symbol_index.get_symbols_for_file(file_path)
    }

    pub fn on_file_created(&mut self, file_path: &str) {
        if !self.is_ready {
            return;
        }
        let Some(relative) = self.to_workspace_relative(file_path) else {
            return;
        };
        if !crate::code_scanner::is_indexable_code_path(&relative) {
            self.remove_stale_indexed_file(&relative);
            return;
        }
        let file_type = classify_file_type(&relative);
        self.ensure_indexed_file_record(&relative, Some(&file_type));
        self.apply_changed_file(&relative, &file_type);
        self.refresh_project_vocabulary_if_needed();
        self.bump_index_version();
        self.save_index();
    }

    pub fn on_file_deleted(&mut self, file_path: &str) {
        if !self.is_ready {
            return;
        }
        let Some(relative) = self.to_workspace_relative(file_path) else {
            return;
        };
        self.apply_deleted_file(&relative);
        self.refresh_project_vocabulary_if_needed();
        self.bump_index_version();
        self.save_index();
    }

    pub fn invalidate_cache(&mut self) {
        self.search_cache.invalidate_all();
    }

    pub fn get_stats(&self) -> SearchEngineStats {
        let idx_stats = self.inverted_index.get_stats();
        let sym_stats = self.symbol_index.get_stats();
        let dep_stats = self.dependency_graph.get_stats();
        let cache_stats = self.search_cache.get_stats();
        SearchEngineStats {
            is_ready: self.is_ready,
            total_documents: idx_stats.total_documents,
            unique_tokens: idx_stats.unique_tokens,
            unique_symbols: sym_stats.unique_symbols,
            total_dep_edges: dep_stats.total_edges,
            cache_hit_rate: cache_stats.hit_rate,
            index_version: self.index_version,
            index_cache_status: self.index_cache_status,
            index_cache_error_code: self.index_cache_error_code,
        }
    }

    pub fn code_index_summary(&self) -> crate::code_scanner::CodeIndexSummary {
        crate::code_scanner::code_index_summary_from_relative_files(
            Path::new(&self.project_root),
            &self.indexed_files,
        )
    }

    fn full_build(&mut self, files: &[(String, String)]) {
        self.inverted_index
            .build_from_files(&self.project_root, files);
        self.symbol_index
            .build_from_files(&self.project_root, files);
        self.dependency_graph
            .build_from_files(&self.project_root, files);
    }

    fn restore_from_snapshot(
        &mut self,
        snapshot: &PersistenceSnapshot,
        freshness: &crate::index_persistence::FreshnessResult,
    ) -> bool {
        self.inverted_index
            .from_snapshot(snapshot.inverted_index.clone());
        self.symbol_index
            .from_snapshot(snapshot.symbol_index.clone());

        let file_set: HashSet<String> = self.indexed_files.iter().map(|(p, _)| p.clone()).collect();
        self.dependency_graph.from_snapshot(
            snapshot.dependency_graph.clone(),
            &self.project_root,
            file_set,
        );

        for file_path in &freshness.deleted {
            self.inverted_index.remove_file(file_path);
            self.symbol_index.remove_file(file_path);
            self.dependency_graph.remove_file(file_path);
        }

        let project_root = self.project_root.clone();
        for file_path in &freshness.modified {
            let file_type = self
                .indexed_files
                .iter()
                .find(|(p, _)| p == file_path)
                .map(|(_, t)| t.as_str())
                .unwrap_or("source");
            self.inverted_index
                .update_file(&project_root, file_path, file_type);
            self.symbol_index.update_file(&project_root, file_path);
            self.dependency_graph.update_file(&project_root, file_path);
        }

        for file_path in &freshness.added {
            let file_type = self
                .indexed_files
                .iter()
                .find(|(p, _)| p == file_path)
                .map(|(_, t)| t.as_str())
                .unwrap_or("source");
            self.inverted_index
                .update_file(&project_root, file_path, file_type);
            self.symbol_index.update_file(&project_root, file_path);
            self.dependency_graph.update_file(&project_root, file_path);
        }

        true
    }

    fn save_index(&mut self) {
        let manifest =
            IndexPersistence::build_file_manifest(&self.project_root, &self.indexed_files);
        let snapshot = PersistenceSnapshot {
            version: 1,
            project_root: self.project_root.clone(),
            created_at: now_millis(),
            updated_at: now_millis(),
            file_manifest: manifest,
            inverted_index: self.inverted_index.to_snapshot(),
            symbol_index: self.symbol_index.to_snapshot(),
            dependency_graph: self.dependency_graph.to_snapshot(),
            expansion_cache: Some(self.query_expander.export_cache()),
        };
        match self.persistence.save(&snapshot) {
            Ok(()) => {
                self.index_cache_status = INDEX_CACHE_STATUS_READY;
                self.index_cache_error_code = None;
            }
            Err(error) => {
                tracing::warn!(
                    project_root = %self.project_root,
                    error = %error,
                    "local search index cache persistence failed"
                );
                self.index_cache_status = INDEX_CACHE_STATUS_DEGRADED;
                self.index_cache_error_code = Some(INDEX_CACHE_SAVE_FAILED_CODE);
            }
        }
    }

    fn apply_changed_file(&mut self, relative_path: &str, file_type: &str) {
        let project_root = self.project_root.clone();
        self.inverted_index
            .update_file(&project_root, relative_path, file_type);
        self.symbol_index.update_file(&project_root, relative_path);
        self.dependency_graph
            .update_file(&project_root, relative_path);
        self.update_tracked_file_state(relative_path);
        self.record_recent_edit(relative_path);
        self.project_vocabulary_dirty = true;
        self.search_cache.invalidate_by_file(relative_path);
    }

    fn apply_deleted_file(&mut self, relative_path: &str) {
        self.indexed_files.retain(|(p, _)| p != relative_path);
        self.tracked_file_states.remove(relative_path);
        self.recent_edited_files.remove(relative_path);
        self.project_vocabulary_dirty = true;

        self.inverted_index.remove_file(relative_path);
        self.symbol_index.remove_file(relative_path);
        self.dependency_graph.remove_file(relative_path);
        self.search_cache.invalidate_by_file(relative_path);
    }

    fn remove_stale_indexed_file(&mut self, relative_path: &str) {
        if !self
            .indexed_files
            .iter()
            .any(|(path, _)| path == relative_path)
        {
            return;
        }
        self.apply_deleted_file(relative_path);
        self.refresh_project_vocabulary_if_needed();
        self.bump_index_version();
        self.save_index();
    }

    fn ensure_indexed_file_record(
        &mut self,
        relative_path: &str,
        preferred_type: Option<&str>,
    ) -> String {
        if let Some(entry) = self.indexed_files.iter().find(|(p, _)| p == relative_path) {
            return entry.1.clone();
        }
        let ft = preferred_type
            .unwrap_or_else(|| classify_file_type_str(relative_path))
            .to_string();
        self.indexed_files
            .push((relative_path.to_string(), ft.clone()));
        ft
    }

    fn rebuild_tracked_file_states(&mut self) {
        self.tracked_file_states.clear();
        let files: Vec<String> = self.indexed_files.iter().map(|(p, _)| p.clone()).collect();
        for file in files {
            self.update_tracked_file_state(&file);
        }
        self.project_vocabulary_dirty = true;
    }

    fn update_tracked_file_state(&mut self, relative_path: &str) {
        let full_path = Path::new(&self.project_root).join(relative_path);
        match std::fs::metadata(&full_path) {
            Ok(meta) => {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                self.tracked_file_states
                    .insert(relative_path.to_string(), (mtime, meta.len()));
            }
            Err(_) => {
                self.tracked_file_states.remove(relative_path);
            }
        }
    }

    fn bump_index_version(&mut self) {
        self.index_version += 1;
        self.search_cache.invalidate_all();
    }

    fn refresh_project_vocabulary_if_needed(&mut self) {
        if !self.project_vocabulary_dirty {
            return;
        }
        let mut vocab = HashSet::new();
        let sym_vocab = self.symbol_index.get_vocabulary(5000);
        for s in sym_vocab {
            vocab.insert(s);
        }
        for (file_path, _) in &self.indexed_files {
            add_path_tokens_to_vocabulary(file_path, &mut vocab);
        }
        self.query_expander.set_project_vocabulary(vocab);
        self.project_vocabulary_dirty = false;
    }

    fn reconcile_indexed_files(&mut self) {
        if !self.is_ready {
            return;
        }
        let files: Vec<(String, String)> = self.indexed_files.clone();
        let mut changed_count = 0;

        for (relative_path, file_type) in &files {
            let full_path = Path::new(&self.project_root).join(relative_path);
            let stat = std::fs::metadata(&full_path).ok();

            if stat.is_none() || !stat.as_ref().unwrap().is_file() {
                self.apply_deleted_file(relative_path);
                changed_count += 1;
                continue;
            }

            let meta = stat.unwrap();
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            let tracked = self.tracked_file_states.get(relative_path);
            let drifted = match tracked {
                None => true,
                Some(&(t_mtime, t_size)) => {
                    (mtime as i64 - t_mtime as i64).unsigned_abs() > 1 || t_size != meta.len()
                }
            };

            if drifted {
                self.apply_changed_file(relative_path, file_type);
                changed_count += 1;
            }
        }

        if changed_count > 0 {
            self.refresh_project_vocabulary_if_needed();
            self.bump_index_version();
        }
    }

    fn record_recent_edit(&mut self, file_path: &str) {
        let now = now_millis();
        self.recent_edited_files.insert(file_path.to_string(), now);
        if self.recent_edited_files.len() > RECENT_EDIT_MAX_FILES {
            let mut sorted: Vec<(String, u64)> = self.recent_edited_files.drain().collect();
            sorted.sort_by(|a, b| b.1.cmp(&a.1));
            sorted.truncate(RECENT_EDIT_MAX_FILES);
            self.recent_edited_files = sorted.into_iter().collect();
        }
    }

    fn get_recent_edited_file_set(&mut self) -> HashSet<String> {
        let now = now_millis();
        let mut recent = HashSet::new();
        self.recent_edited_files
            .retain(|_, ts| now - *ts <= RECENT_EDIT_TTL_MS);
        for (fp, _) in &self.recent_edited_files {
            recent.insert(fp.clone());
        }
        recent
    }

    fn expand_with_dependencies(
        &self,
        ranked: Vec<RankedResult>,
        max_results: usize,
    ) -> Vec<RankedResult> {
        if !self.dependency_graph.is_ready() || ranked.is_empty() {
            return ranked;
        }

        let existing: HashSet<String> = ranked.iter().map(|r| r.file_path.clone()).collect();
        let mut expanded = ranked.clone();
        let top_n = 3.min(ranked.len());

        for i in 0..top_n {
            let top_result = &ranked[i];
            let neighbors = self.dependency_graph.expand(
                &top_result.file_path,
                1,
                crate::dependency_graph::ExpandDirection::Both,
            );

            for neighbor_file in neighbors {
                if existing.contains(&neighbor_file) {
                    continue;
                }
                expanded.push(RankedResult {
                    file_path: neighbor_file.clone(),
                    final_score: top_result.final_score * 0.5,
                    breakdown: ScoreDimensions {
                        tfidf: 0.0,
                        symbol_match: 0.0,
                        position_weight: 0.0,
                        centrality: self.dependency_graph.get_centrality(&neighbor_file),
                        recency: 0.0,
                        type_weight: 0.0,
                        recent_edit_boost: 0.0,
                        scope_boost: 0.0,
                    },
                    sources: vec!["dependency".to_string()],
                });
            }
        }

        expanded.sort_by(|a, b| {
            b.final_score
                .partial_cmp(&a.final_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        expanded.truncate(max_results);
        expanded
    }

    fn assemble_results(
        &self,
        ranked: &[RankedResult],
        index_hits: &[IndexSearchHit],
        symbol_hits: &[SymbolSearchHit],
        max_results: usize,
        max_context_length: usize,
    ) -> Vec<SearchResult> {
        let index_hit_map: HashMap<&str, &IndexSearchHit> = index_hits
            .iter()
            .map(|h| (h.file_path.as_str(), h))
            .collect();

        let mut symbol_line_map: HashMap<&str, Vec<usize>> = HashMap::new();
        for hit in symbol_hits {
            symbol_line_map
                .entry(hit.symbol.file_path.as_str())
                .or_default()
                .push(hit.symbol.line);
        }

        let mut results = Vec::new();
        let mut total_content_length = 0;

        for item in ranked {
            if results.len() >= max_results || total_content_length >= max_context_length {
                break;
            }

            let full_path = Path::new(&self.project_root).join(&item.file_path);
            let lines = match std::fs::read_to_string(&full_path) {
                Ok(content) => content.lines().map(|l| l.to_string()).collect::<Vec<_>>(),
                Err(_) => continue,
            };

            let mut snippets = Vec::new();

            if let Some(index_hit) = index_hit_map.get(item.file_path.as_str()) {
                snippets = extract_snippets(
                    &item.file_path,
                    &lines,
                    &index_hit.hit_lines,
                    &index_hit.matched_tokens,
                    max_context_length - total_content_length,
                    &self.symbol_index,
                );
            }

            if snippets.is_empty() {
                if let Some(sym_lines) = symbol_line_map.get(item.file_path.as_str()) {
                    snippets = extract_snippets(
                        &item.file_path,
                        &lines,
                        sym_lines,
                        &[],
                        max_context_length - total_content_length,
                        &self.symbol_index,
                    );
                }
            }

            let snippet_len: usize = snippets.iter().map(|s| s.content.len()).sum();
            total_content_length += snippet_len;

            results.push(SearchResult {
                file_path: item.file_path.clone(),
                score: item.final_score,
                snippets,
                score_breakdown: ScoreBreakdown {
                    tfidf: item.breakdown.tfidf,
                    symbol_match: item.breakdown.symbol_match,
                    position_weight: item.breakdown.position_weight,
                    centrality: item.breakdown.centrality,
                    recency: item.breakdown.recency,
                    type_weight: item.breakdown.type_weight,
                    recent_edit_boost: item.breakdown.recent_edit_boost,
                    scope_boost: item.breakdown.scope_boost,
                },
            });
        }

        results
    }

    fn to_workspace_relative(&self, file_path: &str) -> Option<String> {
        let path = Path::new(file_path);
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.project_root).ok()?
        } else {
            path
        };
        Some(relative.to_string_lossy().replace('\\', "/"))
    }
}

fn extract_snippets(
    file_path: &str,
    lines: &[String],
    hit_lines: &[usize],
    matched_tokens: &[String],
    max_length: usize,
    symbol_index: &SymbolIndex,
) -> Vec<CodeSnippet> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();

    for &hit_line in hit_lines {
        let (start, end) = if symbol_index.is_ready() {
            if let Some(sym) = symbol_index.get_symbol_at_line(file_path, hit_line) {
                let s = sym.line;
                let e = sym.end_line.unwrap_or(sym.line);
                if e > s && e - s + 1 > 50 {
                    (s.max(hit_line.saturating_sub(25)), e.min(hit_line + 25))
                } else if e > s {
                    (s, e)
                } else {
                    (
                        hit_line.saturating_sub(2),
                        (hit_line + 2).min(lines.len().saturating_sub(1)),
                    )
                }
            } else {
                (
                    hit_line.saturating_sub(2),
                    (hit_line + 2).min(lines.len().saturating_sub(1)),
                )
            }
        } else {
            (
                hit_line.saturating_sub(2),
                (hit_line + 2).min(lines.len().saturating_sub(1)),
            )
        };
        ranges.push((start, end));
    }

    ranges.sort_by_key(|r| r.0);

    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut() {
            if start <= last.1 + 1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }

    let mut snippets = Vec::new();
    let mut total_len = 0;

    for (start, end) in merged {
        if total_len >= max_length {
            break;
        }
        let end_clamped = end.min(lines.len().saturating_sub(1));
        let content: String = lines[start..=end_clamped].join("\n");
        if total_len + content.len() > max_length {
            break;
        }
        total_len += content.len();
        snippets.push(CodeSnippet {
            start_line: start,
            end_line: end_clamped,
            content,
            matched_tokens: matched_tokens.to_vec(),
        });
    }

    snippets
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum QueryIntent {
    Symbol,
    Semantic,
}

fn detect_query_intent(query: &str) -> QueryIntent {
    let trimmed = query.trim();
    if regex::Regex::new(r"^[a-zA-Z_$][a-zA-Z0-9_$]*$")
        .unwrap()
        .is_match(trimmed)
    {
        return QueryIntent::Symbol;
    }
    if trimmed.contains('_')
        && regex::Regex::new(r"^[a-zA-Z_][a-zA-Z0-9_]*$")
            .unwrap()
            .is_match(trimmed)
    {
        return QueryIntent::Symbol;
    }
    QueryIntent::Semantic
}

fn classify_file_type(file_path: &str) -> String {
    classify_file_type_str(file_path).to_string()
}

fn classify_file_type_str(file_path: &str) -> &'static str {
    let lower = file_path.to_lowercase();
    let base = Path::new(&lower)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    if base.contains(".test.")
        || base.contains(".spec.")
        || lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
    {
        return "test";
    }

    let ext = Path::new(&lower)
        .extension()
        .unwrap_or_default()
        .to_string_lossy();

    match ext.as_ref() {
        "json" | "yaml" | "yml" | "toml" | "ini" | "env" | "cfg" => "config",
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "go" | "java" | "rs" | "c" | "h"
        | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "cs" | "php" | "rb" | "swift" | "kt" | "kts"
        | "m" | "mm" | "vue" | "svelte" => "source",
        "md" | "txt" | "rst" => "doc",
        _ => "doc",
    }
}

fn snapshot_project_root_matches(current_project_root: &str, snapshot_project_root: &str) -> bool {
    let current_path = Path::new(current_project_root);
    let snapshot_path = Path::new(snapshot_project_root);
    match (
        std::fs::canonicalize(current_path),
        std::fs::canonicalize(snapshot_path),
    ) {
        (Ok(current), Ok(snapshot)) => current == snapshot,
        _ => {
            normalize_project_root_text(current_project_root)
                == normalize_project_root_text(snapshot_project_root)
        }
    }
}

fn normalize_project_root_text(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(['/', '\\'])
        .replace('\\', "/")
}

fn normalize_scope_hints(scopes: &[String]) -> Vec<String> {
    scopes
        .iter()
        .map(|s| s.trim().replace('\\', "/"))
        .map(|s| s.trim_start_matches("./").to_string())
        .filter(|s| !s.is_empty())
        .take(8)
        .collect()
}

#[derive(Serialize)]
struct SearchCacheKey<'a> {
    query: &'a str,
    max_results: Option<usize>,
    max_context_length: Option<usize>,
    preferred_scopes: Vec<String>,
    prefer_recent_edits: bool,
    llm_expand_result: &'a Option<LlmExpandResult>,
    llm_rerank_response: &'a Option<String>,
}

fn search_cache_key(query: &str, options: &SearchOptions) -> String {
    serde_json::to_string(&SearchCacheKey {
        query,
        max_results: options.max_results,
        max_context_length: options.max_context_length,
        preferred_scopes: normalize_scope_hints(&options.preferred_scopes),
        prefer_recent_edits: options.prefer_recent_edits,
        llm_expand_result: &options.llm_expand_result,
        llm_rerank_response: &options.llm_rerank_response,
    })
    .unwrap_or_else(|_| query.to_string())
}

fn add_path_tokens_to_vocabulary(file_path: &str, vocab: &mut HashSet<String>) {
    let normalized = file_path.replace('\\', "/");
    for part in normalized.split(&['/', '.', '_', '-'][..]) {
        let token = part.trim().to_lowercase();
        if token.len() >= 3
            && token.len() <= 64
            && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            vocab.insert(token);
        }
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_query_intent() {
        assert_eq!(detect_query_intent("getUserProfile"), QueryIntent::Symbol);
        assert_eq!(detect_query_intent("user_auth"), QueryIntent::Symbol);
        assert_eq!(detect_query_intent("用户登录逻辑"), QueryIntent::Semantic);
        assert_eq!(detect_query_intent("how to login"), QueryIntent::Semantic);
    }

    #[test]
    fn test_classify_file_type() {
        assert_eq!(classify_file_type_str("src/auth.ts"), "source");
        assert_eq!(classify_file_type_str("src/auth.test.ts"), "test");
        assert_eq!(classify_file_type_str("config/app.json"), "config");
        assert_eq!(classify_file_type_str("docs/readme.md"), "doc");
        assert_eq!(classify_file_type_str("src/__tests__/auth.ts"), "test");
    }

    #[test]
    fn test_normalize_scope_hints() {
        let scopes = vec!["./src/auth".into(), "  lib/utils  ".into()];
        let normalized = normalize_scope_hints(&scopes);
        assert_eq!(normalized, vec!["src/auth", "lib/utils"]);
    }

    #[test]
    fn test_add_path_tokens() {
        let mut vocab = HashSet::new();
        add_path_tokens_to_vocabulary("src/components/auth-handler.ts", &mut vocab);
        assert!(vocab.contains("src"));
        assert!(vocab.contains("components"));
        assert!(vocab.contains("auth"));
        assert!(vocab.contains("handler"));
    }

    #[test]
    fn test_engine_not_ready_before_build() {
        let engine =
            LocalSearchEngine::new("/tmp/nonexistent_12345", SearchEngineConfig::default());
        assert!(!engine.is_ready());
    }

    #[test]
    fn incremental_updates_keep_scan_indexing_rules() {
        let root = std::env::temp_dir().join(format!(
            "magi-local-search-incremental-filter-{}-{}",
            std::process::id(),
            now_millis()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("create src");
        std::fs::write(
            root.join("src/seed.rs"),
            "pub fn searchable_seed_symbol() -> bool { true }\n",
        )
        .expect("write seed");

        let mut engine = LocalSearchEngine::new(
            root.to_string_lossy().as_ref(),
            SearchEngineConfig::default(),
        );
        engine.build_index(&[("src/seed.rs".to_string(), "source".to_string())]);
        assert_eq!(engine.get_stats().total_documents, 1);

        std::fs::create_dir_all(root.join("target")).expect("create target");
        std::fs::write(
            root.join("target/generated.rs"),
            "pub fn ignored_target_symbol() -> bool { true }\n",
        )
        .expect("write ignored target file");
        std::fs::write(root.join("scratch.txt"), "ignored plain text marker\n")
            .expect("write ignored text file");

        engine.on_file_created(root.join("target/generated.rs").to_string_lossy().as_ref());
        engine.on_file_created(root.join("scratch.txt").to_string_lossy().as_ref());
        assert_eq!(
            engine.get_stats().total_documents,
            1,
            "增量更新必须沿用全量扫描的忽略目录和扩展名规则"
        );

        std::fs::write(
            root.join("src/added.rs"),
            "pub fn accepted_added_symbol() -> bool { true }\n",
        )
        .expect("write accepted file");
        engine.on_file_created(root.join("src/added.rs").to_string_lossy().as_ref());
        assert_eq!(engine.get_stats().total_documents, 2);
        assert!(
            engine
                .search("accepted added symbol", SearchOptions::default())
                .iter()
                .any(|result| result.file_path == "src/added.rs"),
            "正常源码文件仍应被增量索引"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn index_cache_persistence_failure_marks_engine_degraded_without_breaking_search() {
        let root = std::env::temp_dir().join(format!(
            "magi-local-search-cache-degraded-{}-{}",
            std::process::id(),
            now_millis()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("create src");
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn cache_degraded_search_probe() -> bool { true }\n",
        )
        .expect("write source");
        std::fs::write(root.join(".magi"), "occupied").expect("occupy .magi path");

        let mut engine = LocalSearchEngine::new(
            root.to_string_lossy().as_ref(),
            SearchEngineConfig::default(),
        );
        engine.build_index(&[("src/lib.rs".to_string(), "source".to_string())]);
        let stats = engine.get_stats();

        assert!(stats.is_ready);
        assert_eq!(stats.total_documents, 1);
        assert_eq!(stats.index_cache_status, INDEX_CACHE_STATUS_DEGRADED);
        assert_eq!(
            stats.index_cache_error_code,
            Some(INDEX_CACHE_SAVE_FAILED_CODE)
        );
        assert!(
            engine
                .search("cache degraded probe", SearchOptions::default())
                .iter()
                .any(|result| result.file_path == "src/lib.rs"),
            "缓存落盘失败不能破坏当前进程内检索"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn index_cache_restore_rejects_mismatched_project_root() {
        let root = std::env::temp_dir().join(format!(
            "magi-local-search-root-mismatch-{}-{}",
            std::process::id(),
            now_millis()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("create src");
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn freshrootmismatchprobe() -> bool { true }\n",
        )
        .expect("write source");

        write_mismatched_project_root_cache(&root, "src/lib.rs");

        let mut engine = LocalSearchEngine::new(
            root.to_string_lossy().as_ref(),
            SearchEngineConfig::default(),
        );
        engine.build_index(&[("src/lib.rs".to_string(), "source".to_string())]);

        assert!(
            engine
                .search("freshrootmismatchprobe", SearchOptions::default())
                .iter()
                .any(|result| result.file_path == "src/lib.rs"),
            "project_root 不匹配的缓存被丢弃后，应使用当前 workspace 文件重建索引"
        );
        assert!(
            engine
                .search("stalerootmismatchprobe", SearchOptions::default())
                .is_empty(),
            "project_root 不匹配的旧缓存不能污染当前 workspace 检索"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    fn write_mismatched_project_root_cache(root: &Path, file_path: &str) {
        use crate::dependency_graph::DependencyGraphSnapshot;
        use crate::index_persistence::{FileManifestEntry, PersistenceSnapshot};
        use crate::inverted_index::InvertedIndexSnapshot;
        use crate::symbol_index::SymbolIndexSnapshot;
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;

        let full_path = root.join(file_path);
        let meta = std::fs::metadata(&full_path).expect("source metadata");
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let snapshot = PersistenceSnapshot {
            version: 1,
            project_root: "/tmp/magi-different-project-root".to_string(),
            created_at: now_millis(),
            updated_at: now_millis(),
            file_manifest: vec![(
                file_path.to_string(),
                FileManifestEntry {
                    mtime,
                    size: meta.len(),
                    file_type: "source".to_string(),
                },
            )],
            inverted_index: InvertedIndexSnapshot {
                postings: vec![(
                    "stalerootmismatchprobe".to_string(),
                    vec![(file_path.to_string(), vec![0], 1)],
                )],
                doc_meta: vec![(file_path.to_string(), 1, Some(mtime))],
                total_docs: 1,
                avg_doc_len: 1.0,
            },
            symbol_index: SymbolIndexSnapshot {
                symbols: Vec::new(),
                file_symbols: Vec::new(),
            },
            dependency_graph: DependencyGraphSnapshot {
                forward_deps: Vec::new(),
                reverse_deps: Vec::new(),
                edges: Vec::new(),
                centrality_cache: Vec::new(),
            },
            expansion_cache: None,
        };
        let raw = serde_json::to_vec(&snapshot).expect("serialize snapshot");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).expect("write gzip");
        let compressed = encoder.finish().expect("finish gzip");
        let cache_dir = root.join(".magi").join("cache");
        std::fs::create_dir_all(&cache_dir).expect("create cache dir");
        std::fs::write(cache_dir.join("search-index.json.gz"), compressed).expect("write cache");
    }
}
