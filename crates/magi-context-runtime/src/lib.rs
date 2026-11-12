mod budgeting;
mod execution_context;
mod knowledge_context;
mod source_assembly;
mod structured_output;

use magi_core::{SessionId, UtcMillis, WorkspaceId};
use magi_knowledge_store::{GovernedKnowledgeOutput, KnowledgeQuery, KnowledgeStore};
use magi_memory_store::{MemoryQuery, MemoryRecord, MemoryStore};
use magi_session_store::SessionStore;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, RwLock},
};

pub use execution_context::{ExecutionContextAssemblyRequest, ExecutionContextClues};
pub use knowledge_context::{
    KnowledgeConsumer, KnowledgeContextDecision, KnowledgeContextRequest,
    KnowledgeContextSelection, KnowledgeGraphContext, KnowledgeGraphContextEdge,
    KnowledgeGraphContextNode, SelectedKnowledgeContext,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextBudget {
    pub max_turns: usize,
    pub max_knowledge: usize,
    pub max_memory: usize,
    pub max_shared_items: usize,
    pub max_file_summaries: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SharedContextItem {
    pub item_id: String,
    pub title: String,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SharedContextRecord {
    pub item: SharedContextItem,
    pub session_id: Option<SessionId>,
    pub project_key: Option<String>,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SharedContextQuery {
    pub session_id: Option<SessionId>,
    pub project_key: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SharedContextPool {
    entries: Arc<RwLock<HashMap<SharedContextKey, SharedContextRecord>>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SharedContextKey {
    session_id: Option<SessionId>,
    project_key: Option<String>,
    item_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileSummaryItem {
    pub absolute_path: String,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileSummaryRecord {
    pub item: FileSummaryItem,
    pub workspace_id: Option<WorkspaceId>,
    pub project_key: Option<String>,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileSummaryQuery {
    pub workspace_id: Option<WorkspaceId>,
    pub project_key: Option<String>,
    pub path_prefix: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Default)]
pub struct FileSummaryStore {
    entries: Arc<RwLock<HashMap<FileSummaryKey, FileSummaryRecord>>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FileSummaryKey {
    workspace_id: Option<WorkspaceId>,
    project_key: Option<String>,
    absolute_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectRecentTurnRecord {
    pub turn_id: String,
    pub project_key: String,
    pub content: String,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectRecentTurnsQuery {
    pub project_key: String,
    pub limit: usize,
}

#[derive(Clone, Debug, Default)]
pub struct ProjectRecentTurnStore {
    entries: Arc<RwLock<HashMap<ProjectRecentTurnKey, ProjectRecentTurnRecord>>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProjectRecentTurnKey {
    project_key: String,
    turn_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextAssemblyInput {
    pub recent_turns: Vec<String>,
    pub knowledge_query: KnowledgeQuery,
    pub memory_query: MemoryQuery,
    pub shared_context: Vec<SharedContextItem>,
    pub file_summaries: Vec<FileSummaryItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecentTurnSource {
    Session,
    Project,
    Provided,
}

impl RecentTurnSource {
    fn priority(self) -> u8 {
        match self {
            RecentTurnSource::Provided => 0,
            RecentTurnSource::Project => 1,
            RecentTurnSource::Session => 2,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentTurnRecord {
    pub source: RecentTurnSource,
    pub content: String,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentTurnsResolutionSummary {
    pub resolved_count: usize,
    pub retained_count: usize,
    pub session_source_count: usize,
    pub project_source_count: usize,
    pub provided_source_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentTurnsSourceQuery {
    pub session_id: Option<SessionId>,
    pub project_key: Option<String>,
    pub limit: usize,
    pub max_session_turns: Option<usize>,
    pub max_project_turns: Option<usize>,
    pub deduplicate: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextSourceAssemblyInput {
    pub recent_turns_query: RecentTurnsSourceQuery,
    pub knowledge_query: KnowledgeQuery,
    pub memory_query: MemoryQuery,
    pub shared_context_query: SharedContextQuery,
    pub file_summary_query: FileSummaryQuery,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TruncationRecord {
    pub part: String,
    pub original_count: usize,
    pub retained_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextBudgetUsage {
    pub used_turns: usize,
    pub used_knowledge: usize,
    pub used_memory: usize,
    pub used_shared_items: usize,
    pub used_file_summaries: usize,
    pub truncations: Vec<TruncationRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextAssemblyResult {
    pub selected_recent_turns: Vec<RecentTurnRecord>,
    pub selected_turns: Vec<String>,
    pub selected_knowledge: Vec<GovernedKnowledgeOutput>,
    pub selected_memory: Vec<MemoryRecord>,
    pub selected_shared_context: Vec<SharedContextItem>,
    pub selected_file_summaries: Vec<FileSummaryItem>,
    pub recent_turns_summary: RecentTurnsResolutionSummary,
    pub usage: ContextBudgetUsage,
}

#[derive(Clone, Debug)]
pub struct ContextRuntime {
    knowledge_store: KnowledgeStore,
    memory_store: MemoryStore,
    session_store: Option<SessionStore>,
    shared_context_pool: SharedContextPool,
    file_summary_store: FileSummaryStore,
    project_recent_turn_store: ProjectRecentTurnStore,
    knowledge_selection_cache: KnowledgeSelectionCache,
}

#[derive(Clone, Debug, Default)]
struct KnowledgeSelectionCache {
    state: Arc<Mutex<KnowledgeSelectionCacheState>>,
}

#[derive(Debug, Default)]
struct KnowledgeSelectionCacheState {
    entries: HashMap<String, KnowledgeContextSelection>,
    order: VecDeque<String>,
}

const KNOWLEDGE_SELECTION_CACHE_LIMIT: usize = 256;

impl ContextRuntime {
    pub fn new(knowledge_store: KnowledgeStore, memory_store: MemoryStore) -> Self {
        Self {
            knowledge_store,
            memory_store,
            session_store: None,
            shared_context_pool: SharedContextPool::default(),
            file_summary_store: FileSummaryStore::default(),
            project_recent_turn_store: ProjectRecentTurnStore::default(),
            knowledge_selection_cache: KnowledgeSelectionCache::default(),
        }
    }

    pub fn with_runtime_sources(
        knowledge_store: KnowledgeStore,
        memory_store: MemoryStore,
        session_store: SessionStore,
        shared_context_pool: SharedContextPool,
        file_summary_store: FileSummaryStore,
        project_recent_turn_store: ProjectRecentTurnStore,
    ) -> Self {
        Self {
            knowledge_store,
            memory_store,
            session_store: Some(session_store),
            shared_context_pool,
            file_summary_store,
            project_recent_turn_store,
            knowledge_selection_cache: KnowledgeSelectionCache::default(),
        }
    }

    pub fn shared_context_pool(&self) -> SharedContextPool {
        self.shared_context_pool.clone()
    }

    pub fn file_summary_store(&self) -> FileSummaryStore {
        self.file_summary_store.clone()
    }

    pub fn project_recent_turn_store(&self) -> ProjectRecentTurnStore {
        self.project_recent_turn_store.clone()
    }

    pub fn assemble(
        &self,
        budget: &ContextBudget,
        input: ContextAssemblyInput,
    ) -> ContextAssemblyResult {
        let (recent_turns, recent_turns_summary) =
            source_assembly::provided_recent_turns(input.recent_turns);
        self.assemble_internal(
            budget,
            budgeting::ContextSelectionInput {
                recent_turns,
                recent_turns_summary,
                knowledge_query: input.knowledge_query,
                memory_query: input.memory_query,
                shared_context: input.shared_context,
                file_summaries: input.file_summaries,
            },
        )
    }

    pub fn assemble_from_runtime_sources(
        &self,
        budget: &ContextBudget,
        input: ContextSourceAssemblyInput,
    ) -> ContextAssemblyResult {
        let resolved_sources = source_assembly::resolve_runtime_sources(
            self,
            &input.recent_turns_query,
            &input.shared_context_query,
            &input.file_summary_query,
        );

        self.assemble_internal(
            budget,
            budgeting::ContextSelectionInput {
                recent_turns: resolved_sources.recent_turns,
                recent_turns_summary: resolved_sources.recent_turns_summary,
                knowledge_query: input.knowledge_query,
                memory_query: input.memory_query,
                shared_context: resolved_sources.shared_context,
                file_summaries: resolved_sources.file_summaries,
            },
        )
    }

    fn assemble_internal(
        &self,
        budget: &ContextBudget,
        input: budgeting::ContextSelectionInput,
    ) -> ContextAssemblyResult {
        let selection = budgeting::assemble_budgeted_selection(self, budget, input);
        structured_output::build_context_assembly_result(selection)
    }
}

impl KnowledgeSelectionCache {
    fn get(&self, key: &str) -> Option<KnowledgeContextSelection> {
        let mut state = self
            .state
            .lock()
            .expect("knowledge selection cache poisoned");
        let value = state.entries.get(key).cloned()?;
        state.order.retain(|item| item != key);
        state.order.push_back(key.to_string());
        Some(value)
    }

    fn insert(&self, key: String, value: KnowledgeContextSelection) {
        let mut state = self
            .state
            .lock()
            .expect("knowledge selection cache poisoned");
        state.entries.insert(key.clone(), value);
        state.order.retain(|item| item != &key);
        state.order.push_back(key);
        while state.order.len() > KNOWLEDGE_SELECTION_CACHE_LIMIT {
            if let Some(oldest) = state.order.pop_front() {
                state.entries.remove(&oldest);
            }
        }
    }
}

impl SharedContextPool {
    pub fn upsert(&self, record: SharedContextRecord) {
        self.entries
            .write()
            .expect("shared context pool write lock poisoned")
            .insert(SharedContextKey::from_record(&record), record);
    }

    pub fn query(&self, query: &SharedContextQuery) -> Vec<SharedContextRecord> {
        let mut records = self
            .entries
            .read()
            .expect("shared context pool read lock poisoned")
            .values()
            .filter(|record| shared_context_scope_matches(query, record))
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            right
                .updated_at
                .0
                .cmp(&left.updated_at.0)
                .then_with(|| left.item.item_id.cmp(&right.item.item_id))
        });
        if query.limit < records.len() {
            records.truncate(query.limit);
        }
        records
    }
}

impl SharedContextKey {
    fn from_record(record: &SharedContextRecord) -> Self {
        Self {
            session_id: record.session_id.clone(),
            project_key: record.project_key.clone(),
            item_id: record.item.item_id.clone(),
        }
    }
}

impl FileSummaryStore {
    pub fn upsert(&self, record: FileSummaryRecord) {
        self.entries
            .write()
            .expect("file summary store write lock poisoned")
            .insert(FileSummaryKey::from_record(&record), record);
    }

    /// 以 transcript 派生结果整体替换指定作用域。该存储是可重建投影，不是事实源。
    pub fn replace_scope(
        &self,
        workspace_id: Option<&WorkspaceId>,
        project_key: Option<&str>,
        records: Vec<FileSummaryRecord>,
    ) {
        let mut entries = self
            .entries
            .write()
            .expect("file summary store write lock poisoned");
        entries.retain(|_, record| {
            record.workspace_id.as_ref() != workspace_id
                || record.project_key.as_deref() != project_key
        });
        for record in records {
            entries.insert(FileSummaryKey::from_record(&record), record);
        }
    }

    pub fn query(&self, query: &FileSummaryQuery) -> Vec<FileSummaryRecord> {
        let mut records =
            self.entries
                .read()
                .expect("file summary store read lock poisoned")
                .values()
                .filter(|record| file_summary_scope_matches(query, record))
                .filter(|record| {
                    query.path_prefix.as_ref().is_none_or(|path_prefix| {
                        record.item.absolute_path.starts_with(path_prefix)
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            right
                .updated_at
                .0
                .cmp(&left.updated_at.0)
                .then_with(|| left.item.absolute_path.cmp(&right.item.absolute_path))
        });
        if query.limit < records.len() {
            records.truncate(query.limit);
        }
        records
    }
}

impl FileSummaryKey {
    fn from_record(record: &FileSummaryRecord) -> Self {
        Self {
            workspace_id: record.workspace_id.clone(),
            project_key: record.project_key.clone(),
            absolute_path: record.item.absolute_path.clone(),
        }
    }
}

fn shared_context_scope_matches(query: &SharedContextQuery, record: &SharedContextRecord) -> bool {
    let session_matches = query
        .session_id
        .as_ref()
        .is_some_and(|session_id| record.session_id.as_ref() == Some(session_id));
    let project_matches = query
        .project_key
        .as_ref()
        .is_some_and(|project_key| record.project_key.as_ref() == Some(project_key));
    let has_scope_query = query.session_id.is_some() || query.project_key.is_some();

    if !has_scope_query {
        return record.session_id.is_none() && record.project_key.is_none();
    }
    if record.session_id.is_some() && !session_matches {
        return false;
    }
    if let Some(project_key) = query.project_key.as_ref()
        && record
            .project_key
            .as_ref()
            .is_some_and(|record_project| record_project != project_key)
    {
        return false;
    }
    session_matches || project_matches
}

fn file_summary_scope_matches(query: &FileSummaryQuery, record: &FileSummaryRecord) -> bool {
    if let Some(workspace_id) = query.workspace_id.as_ref() {
        if record.workspace_id.as_ref() != Some(workspace_id) {
            return false;
        }
        if let Some(project_key) = query.project_key.as_ref()
            && record
                .project_key
                .as_ref()
                .is_some_and(|record_project| record_project != project_key)
        {
            return false;
        }
        return true;
    }

    if let Some(project_key) = query.project_key.as_ref() {
        return record.workspace_id.is_none() && record.project_key.as_ref() == Some(project_key);
    }

    record.workspace_id.is_none() && record.project_key.is_none()
}

impl ProjectRecentTurnStore {
    pub fn upsert(&self, record: ProjectRecentTurnRecord) {
        self.entries
            .write()
            .expect("project recent turn store write lock poisoned")
            .insert(ProjectRecentTurnKey::from_record(&record), record);
    }

    pub fn query(&self, query: &ProjectRecentTurnsQuery) -> Vec<ProjectRecentTurnRecord> {
        let mut records = self
            .entries
            .read()
            .expect("project recent turn store read lock poisoned")
            .values()
            .filter(|record| record.project_key == query.project_key)
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            right
                .updated_at
                .0
                .cmp(&left.updated_at.0)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        if query.limit < records.len() {
            records.truncate(query.limit);
        }
        records
    }
}

impl ProjectRecentTurnKey {
    fn from_record(record: &ProjectRecentTurnRecord) -> Self {
        Self {
            project_key: record.project_key.clone(),
            turn_id: record.turn_id.clone(),
        }
    }
}

#[cfg(test)]
mod tests;
