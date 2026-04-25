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

#[cfg(test)]
mod tests;

use magi_core::{DomainError, UtcMillis, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

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

#[derive(Clone, Debug, Default)]
pub struct KnowledgeStore {
    state: Arc<RwLock<KnowledgeState>>,
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
        }
    }

    pub fn export_state(&self) -> KnowledgeState {
        self.state
            .read()
            .expect("knowledge store read lock poisoned")
            .clone()
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
        GovernedKnowledgeService::project(
            query_result,
            &state.code_sources,
            &state.audit_links,
            &state.governance_links,
        )
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
