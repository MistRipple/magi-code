mod budgeting;
mod execution_context;
pub mod layered_memory_store;
pub mod memory_document;
pub mod shared_context_pool;
mod source_assembly;
mod structured_output;

use magi_core::{SessionId, UtcMillis, WorkspaceId};
use magi_knowledge_store::{GovernedKnowledgeOutput, KnowledgeQuery, KnowledgeStore};
use magi_memory_store::{MemoryQuery, MemoryRecord, MemoryStore};
use magi_session_store::SessionStore;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

pub use execution_context::{ExecutionContextAssemblyRequest, ExecutionContextClues};

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
}

impl ContextRuntime {
    pub fn new(knowledge_store: KnowledgeStore, memory_store: MemoryStore) -> Self {
        Self {
            knowledge_store,
            memory_store,
            session_store: None,
            shared_context_pool: SharedContextPool::default(),
            file_summary_store: FileSummaryStore::default(),
            project_recent_turn_store: ProjectRecentTurnStore::default(),
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
            recent_turns,
            recent_turns_summary,
            input.knowledge_query,
            input.memory_query,
            input.shared_context,
            input.file_summaries,
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
            resolved_sources.recent_turns,
            resolved_sources.recent_turns_summary,
            input.knowledge_query,
            input.memory_query,
            resolved_sources.shared_context,
            resolved_sources.file_summaries,
        )
    }

    fn assemble_internal(
        &self,
        budget: &ContextBudget,
        recent_turns: Vec<RecentTurnRecord>,
        recent_turns_summary: RecentTurnsResolutionSummary,
        knowledge_query: KnowledgeQuery,
        memory_query: MemoryQuery,
        shared_context: Vec<SharedContextItem>,
        file_summaries: Vec<FileSummaryItem>,
    ) -> ContextAssemblyResult {
        let selection = budgeting::assemble_budgeted_selection(
            self,
            budget,
            recent_turns,
            recent_turns_summary,
            knowledge_query,
            memory_query,
            shared_context,
            file_summaries,
        );
        structured_output::build_context_assembly_result(selection)
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
mod tests {
    use super::*;
    use magi_core::{SessionId, UtcMillis};
    use magi_session_store::{
        SessionExecutionSidecarStoreState, SessionRecord, SessionStore, SessionStoreState,
        TimelineEntry, TimelineEntryKind,
    };

    fn empty_context_runtime() -> ContextRuntime {
        ContextRuntime::new(KnowledgeStore::new(), MemoryStore::new())
    }

    #[test]
    fn assemble_from_runtime_sources_keeps_recent_turn_source_information() {
        let session_id = SessionId::new("session-1");
        let project_key = "project-a".to_string();
        let timestamp = UtcMillis(42);

        let session_store = SessionStore::from_state(SessionStoreState {
            current_session_id: Some(session_id.clone()),
            sessions: vec![SessionRecord {
                session_id: session_id.clone(),
                title: "Session".to_string(),
                status: magi_core::SessionLifecycleStatus::Active,
                created_at: timestamp,
                updated_at: timestamp,
                message_count: None,
                workspace_id: None,
            }],
            timeline: vec![TimelineEntry {
                entry_id: "timeline-session-1".to_string(),
                session_id: session_id.clone(),
                kind: TimelineEntryKind::SystemNote,
                message: "same turn".to_string(),
                occurred_at: timestamp,
            }],
            notifications: vec![],
            canonical_turns: vec![],
            thread_registry: vec![],
            execution_sidecar_store: SessionExecutionSidecarStoreState {
                runtime_sidecars: vec![],
            },
        });
        let project_recent_turn_store = ProjectRecentTurnStore::default();
        project_recent_turn_store.upsert(ProjectRecentTurnRecord {
            turn_id: "project-turn-1".to_string(),
            project_key: project_key.clone(),
            content: "same turn".to_string(),
            updated_at: timestamp,
        });

        let runtime = ContextRuntime::with_runtime_sources(
            KnowledgeStore::new(),
            MemoryStore::new(),
            session_store,
            SharedContextPool::default(),
            FileSummaryStore::default(),
            project_recent_turn_store,
        );

        let result = runtime.assemble_from_runtime_sources(
            &ContextBudget {
                max_turns: 8,
                max_knowledge: 0,
                max_memory: 0,
                max_shared_items: 0,
                max_file_summaries: 0,
            },
            ContextSourceAssemblyInput {
                recent_turns_query: RecentTurnsSourceQuery {
                    session_id: Some(session_id),
                    project_key: Some(project_key),
                    limit: 8,
                    max_session_turns: Some(8),
                    max_project_turns: Some(8),
                    deduplicate: true,
                },
                knowledge_query: KnowledgeQuery {
                    workspace_id: None,
                    kind: None,
                    text: None,
                    tags: vec![],
                    limit: 0,
                },
                memory_query: MemoryQuery {
                    session_id: SessionId::new("session-1"),
                    layer: None,
                    limit: 0,
                },
                shared_context_query: SharedContextQuery {
                    session_id: None,
                    project_key: None,
                    limit: 0,
                },
                file_summary_query: FileSummaryQuery {
                    workspace_id: None,
                    project_key: None,
                    path_prefix: None,
                    limit: 0,
                },
            },
        );

        assert_eq!(result.selected_recent_turns.len(), 1);
        assert!(matches!(
            result.selected_recent_turns[0].source,
            RecentTurnSource::Session
        ));
        assert_eq!(result.selected_turns, vec!["same turn".to_string()]);
        assert_eq!(result.recent_turns_summary.resolved_count, 1);
        assert_eq!(result.recent_turns_summary.retained_count, 1);
        assert_eq!(result.recent_turns_summary.session_source_count, 1);
        assert_eq!(result.recent_turns_summary.project_source_count, 0);
        assert_eq!(result.recent_turns_summary.provided_source_count, 0);
    }

    #[test]
    fn direct_assembly_marks_recent_turns_as_provided() {
        let runtime = empty_context_runtime();
        let result = runtime.assemble(
            &ContextBudget {
                max_turns: 2,
                max_knowledge: 0,
                max_memory: 0,
                max_shared_items: 0,
                max_file_summaries: 0,
            },
            ContextAssemblyInput {
                recent_turns: vec!["manual turn".to_string()],
                knowledge_query: KnowledgeQuery {
                    workspace_id: None,
                    kind: None,
                    text: None,
                    tags: vec![],
                    limit: 0,
                },
                memory_query: MemoryQuery {
                    session_id: SessionId::new("session-1"),
                    layer: None,
                    limit: 0,
                },
                shared_context: vec![],
                file_summaries: vec![],
            },
        );

        assert_eq!(result.selected_recent_turns.len(), 1);
        assert!(matches!(
            result.selected_recent_turns[0].source,
            RecentTurnSource::Provided
        ));
        assert_eq!(result.selected_turns, vec!["manual turn".to_string()]);
        assert_eq!(result.recent_turns_summary.resolved_count, 1);
        assert_eq!(result.recent_turns_summary.provided_source_count, 1);
    }

    #[test]
    fn full_five_source_assembly_with_budget_truncation() {
        let session_id = SessionId::new("session-1");
        let project_key = "project-a".to_string();
        let workspace_id = WorkspaceId::new("workspace-1");
        let ts = |v: u64| UtcMillis(v);

        // --- knowledge store: 4 records, budget allows 2 ---
        let knowledge_store = KnowledgeStore::new();
        for i in 1..=4 {
            knowledge_store.upsert(magi_knowledge_store::KnowledgeRecord {
                workspace_id: None,
                knowledge_id: format!("kb-{i}"),
                kind: magi_knowledge_store::KnowledgeKind::Faq,
                title: format!("Knowledge item {i}"),
                content: format!("knowledge content about topic {i}"),
                tags: vec!["rust".to_string()],
                source_ref: Some(format!("ref-{i}")),
                updated_at: ts(100 + i),
            });
        }

        // --- memory store: 5 records, budget allows 2 ---
        let memory_store = MemoryStore::new();
        for i in 1..=5 {
            memory_store.append(MemoryRecord {
                memory_id: format!("mem-{i}"),
                session_id: session_id.clone(),
                layer: magi_memory_store::MemoryLayer::Recent,
                content: format!("memory content {i}"),
                provenance: None,
                compacted: false,
                created_at: ts(200 + i),
            });
        }

        // --- session store: 3 timeline entries ---
        let session_store = SessionStore::from_state(SessionStoreState {
            current_session_id: Some(session_id.clone()),
            sessions: vec![SessionRecord {
                session_id: session_id.clone(),
                title: "Test session".to_string(),
                status: magi_core::SessionLifecycleStatus::Active,
                created_at: ts(1),
                updated_at: ts(1),
                message_count: None,
                workspace_id: None,
            }],
            timeline: (1..=3)
                .map(|i| TimelineEntry {
                    entry_id: format!("timeline-{i}"),
                    session_id: session_id.clone(),
                    kind: TimelineEntryKind::SystemNote,
                    message: format!("session turn {i}"),
                    occurred_at: ts(300 + i),
                })
                .collect(),
            notifications: vec![],
            canonical_turns: vec![],
            thread_registry: vec![],
            execution_sidecar_store: SessionExecutionSidecarStoreState {
                runtime_sidecars: vec![],
            },
        });

        // --- project recent turns: 2 entries ---
        let project_recent_turn_store = ProjectRecentTurnStore::default();
        for i in 1..=2 {
            project_recent_turn_store.upsert(ProjectRecentTurnRecord {
                turn_id: format!("project-turn-{i}"),
                project_key: project_key.clone(),
                content: format!("project turn {i}"),
                updated_at: ts(400 + i),
            });
        }

        // --- shared context pool: 4 items ---
        let shared_context_pool = SharedContextPool::default();
        for i in 1..=4 {
            shared_context_pool.upsert(SharedContextRecord {
                item: SharedContextItem {
                    item_id: format!("shared-{i}"),
                    title: format!("Shared context {i}"),
                    content: format!("shared content {i}"),
                },
                session_id: Some(session_id.clone()),
                project_key: Some(project_key.clone()),
                updated_at: ts(500 + i),
            });
        }

        // --- file summary store: 3 items ---
        let file_summary_store = FileSummaryStore::default();
        for i in 1..=3 {
            file_summary_store.upsert(FileSummaryRecord {
                item: FileSummaryItem {
                    absolute_path: format!("/src/file_{i}.rs"),
                    summary: format!("File {i} summary"),
                },
                workspace_id: Some(workspace_id.clone()),
                project_key: Some(project_key.clone()),
                updated_at: ts(600 + i),
            });
        }

        let runtime = ContextRuntime::with_runtime_sources(
            knowledge_store,
            memory_store,
            session_store,
            shared_context_pool,
            file_summary_store,
            project_recent_turn_store,
        );

        // budget: turns=3, knowledge=2, memory=2, shared=2, files=1
        let budget = ContextBudget {
            max_turns: 3,
            max_knowledge: 2,
            max_memory: 2,
            max_shared_items: 2,
            max_file_summaries: 1,
        };

        let result = runtime.assemble_from_runtime_sources(
            &budget,
            ContextSourceAssemblyInput {
                recent_turns_query: RecentTurnsSourceQuery {
                    session_id: Some(session_id.clone()),
                    project_key: Some(project_key.clone()),
                    limit: 10,
                    max_session_turns: None,
                    max_project_turns: None,
                    deduplicate: false,
                },
                knowledge_query: KnowledgeQuery {
                    workspace_id: None,
                    kind: None,
                    text: Some("topic".to_string()),
                    tags: vec!["rust".to_string()],
                    limit: 10,
                },
                memory_query: MemoryQuery {
                    session_id: session_id.clone(),
                    layer: None,
                    limit: 10,
                },
                shared_context_query: SharedContextQuery {
                    session_id: Some(session_id.clone()),
                    project_key: Some(project_key.clone()),
                    limit: 10,
                },
                file_summary_query: FileSummaryQuery {
                    workspace_id: Some(workspace_id.clone()),
                    project_key: Some(project_key.clone()),
                    path_prefix: None,
                    limit: 10,
                },
            },
        );

        // --- turns: 5 resolved (3 session + 2 project), budget truncates to 3 ---
        assert_eq!(result.selected_recent_turns.len(), 3);
        assert_eq!(result.selected_turns.len(), 3);
        assert_eq!(result.recent_turns_summary.resolved_count, 5);
        assert_eq!(result.recent_turns_summary.retained_count, 3);

        // --- knowledge: 4 matched, budget truncates to 2 ---
        assert_eq!(result.selected_knowledge.len(), 2);

        // --- memory: 5 total, budget truncates to 2 ---
        assert_eq!(result.selected_memory.len(), 2);

        // --- shared context: 4 total, budget truncates to 2 ---
        assert_eq!(result.selected_shared_context.len(), 2);

        // --- file summaries: 3 total, budget truncates to 1 ---
        assert_eq!(result.selected_file_summaries.len(), 1);

        // --- usage counters ---
        assert_eq!(result.usage.used_turns, 3);
        assert_eq!(result.usage.used_knowledge, 2);
        assert_eq!(result.usage.used_memory, 2);
        assert_eq!(result.usage.used_shared_items, 2);
        assert_eq!(result.usage.used_file_summaries, 1);

        // --- truncation records: all 5 sources are truncated ---
        assert_eq!(result.usage.truncations.len(), 5);
        let truncation_map: HashMap<String, (usize, usize)> = result
            .usage
            .truncations
            .iter()
            .map(|t| (t.part.clone(), (t.original_count, t.retained_count)))
            .collect();
        assert_eq!(truncation_map["recent_turns"], (5, 3));
        assert_eq!(truncation_map["knowledge"], (4, 2));
        assert_eq!(truncation_map["memory"], (5, 2));
        assert_eq!(truncation_map["shared_context"], (4, 2));
        assert_eq!(truncation_map["file_summaries"], (3, 1));
    }

    #[test]
    fn execution_context_request_maps_ids_budget_and_text_clues_into_source_queries() {
        let session_id = SessionId::new("session-exec");
        let workspace_id = WorkspaceId::new("workspace-exec");
        let request = ExecutionContextAssemblyRequest {
            session_id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            project_key: Some("project-alpha".to_string()),
            clues: ExecutionContextClues {
                mission: Some("Stabilize parser cutover".to_string()),
                assignment: Some("Investigate manifest fallback".to_string()),
                task: Some("Finalize parser diagnostics".to_string()),
            },
            budget: ContextBudget {
                max_turns: 4,
                max_knowledge: 3,
                max_memory: 2,
                max_shared_items: 1,
                max_file_summaries: 2,
            },
        };

        let source_input = request.to_source_assembly_input();

        assert_eq!(
            source_input.recent_turns_query.session_id,
            Some(session_id.clone())
        );
        assert_eq!(
            source_input.recent_turns_query.project_key.as_deref(),
            Some("project-alpha")
        );
        assert_eq!(source_input.recent_turns_query.limit, 4);
        assert_eq!(source_input.recent_turns_query.max_session_turns, Some(4));
        assert_eq!(source_input.recent_turns_query.max_project_turns, Some(4));
        assert!(source_input.recent_turns_query.deduplicate);

        assert_eq!(
            source_input.knowledge_query.text.as_deref(),
            Some(
                "Stabilize parser cutover\nInvestigate manifest fallback\nFinalize parser diagnostics"
            )
        );
        assert_eq!(source_input.knowledge_query.limit, 3);
        assert_eq!(source_input.memory_query.session_id, session_id);
        assert_eq!(source_input.memory_query.limit, 2);
        assert_eq!(
            source_input.shared_context_query.project_key.as_deref(),
            Some("project-alpha")
        );
        assert_eq!(source_input.shared_context_query.limit, 1);
        assert_eq!(
            source_input.file_summary_query.workspace_id,
            Some(workspace_id)
        );
        assert_eq!(source_input.file_summary_query.limit, 2);
    }

    #[test]
    fn assemble_execution_context_uses_text_clues_and_runtime_sources() {
        let session_id = SessionId::new("session-exec");
        let workspace_id = WorkspaceId::new("workspace-exec");
        let project_key = "project-alpha".to_string();
        let timestamp = |value: u64| UtcMillis(value);

        let knowledge_store = KnowledgeStore::new();
        knowledge_store.upsert(magi_knowledge_store::KnowledgeRecord {
            workspace_id: Some(workspace_id.clone()),
            knowledge_id: "kb-parser-diagnostics".to_string(),
            kind: magi_knowledge_store::KnowledgeKind::Adr,
            title: "Parser diagnostics cutover notes".to_string(),
            content: "Stabilize parser diagnostics before the cutover fallback path.".to_string(),
            tags: vec!["parser".to_string()],
            source_ref: Some("docs/parser-cutover.md".to_string()),
            updated_at: timestamp(10),
        });

        let memory_store = MemoryStore::new();
        memory_store.append(MemoryRecord {
            memory_id: "mem-session-1".to_string(),
            session_id: session_id.clone(),
            layer: magi_memory_store::MemoryLayer::Recent,
            content: "Remember to keep diagnostics stable during rollout.".to_string(),
            provenance: None,
            compacted: false,
            created_at: timestamp(20),
        });

        let session_store = SessionStore::from_state(SessionStoreState {
            current_session_id: Some(session_id.clone()),
            sessions: vec![SessionRecord {
                session_id: session_id.clone(),
                title: "Execution session".to_string(),
                status: magi_core::SessionLifecycleStatus::Active,
                created_at: timestamp(1),
                updated_at: timestamp(1),
                message_count: None,
                workspace_id: None,
            }],
            timeline: vec![TimelineEntry {
                entry_id: "timeline-exec-1".to_string(),
                session_id: session_id.clone(),
                kind: TimelineEntryKind::SystemNote,
                message: "Session turn about parser diagnostics".to_string(),
                occurred_at: timestamp(30),
            }],
            notifications: vec![],
            canonical_turns: vec![],
            thread_registry: vec![],
            execution_sidecar_store: SessionExecutionSidecarStoreState {
                runtime_sidecars: vec![],
            },
        });

        let shared_context_pool = SharedContextPool::default();
        shared_context_pool.upsert(SharedContextRecord {
            item: SharedContextItem {
                item_id: "shared-exec-1".to_string(),
                title: "Execution checklist".to_string(),
                content: "Track parser diagnostics and cutover safety checks.".to_string(),
            },
            session_id: Some(session_id.clone()),
            project_key: Some(project_key.clone()),
            updated_at: timestamp(40),
        });

        let file_summary_store = FileSummaryStore::default();
        file_summary_store.upsert(FileSummaryRecord {
            item: FileSummaryItem {
                absolute_path: "/repo/src/parser.rs".to_string(),
                summary: "Parser entrypoint and diagnostics plumbing.".to_string(),
            },
            workspace_id: Some(workspace_id.clone()),
            project_key: Some(project_key.clone()),
            updated_at: timestamp(50),
        });

        let project_recent_turn_store = ProjectRecentTurnStore::default();
        project_recent_turn_store.upsert(ProjectRecentTurnRecord {
            turn_id: "project-turn-exec-1".to_string(),
            project_key: project_key.clone(),
            content: "Project turn for parser fallback review".to_string(),
            updated_at: timestamp(60),
        });

        let runtime = ContextRuntime::with_runtime_sources(
            knowledge_store,
            memory_store,
            session_store,
            shared_context_pool,
            file_summary_store,
            project_recent_turn_store,
        );

        let result = runtime.assemble_execution_context(&ExecutionContextAssemblyRequest {
            session_id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            project_key: Some(project_key.clone()),
            clues: ExecutionContextClues {
                mission: Some("Stabilize parser cutover".to_string()),
                assignment: Some("Review fallback path".to_string()),
                task: Some("Finalize parser diagnostics".to_string()),
            },
            budget: ContextBudget {
                max_turns: 4,
                max_knowledge: 3,
                max_memory: 3,
                max_shared_items: 2,
                max_file_summaries: 2,
            },
        });

        assert_eq!(result.selected_knowledge.len(), 1);
        assert_eq!(
            result.selected_knowledge[0].knowledge_id,
            "kb-parser-diagnostics"
        );
        assert!(
            result.selected_knowledge[0]
                .matched_terms
                .iter()
                .any(|term| term == "parser")
        );
        assert!(
            result.selected_knowledge[0]
                .matched_terms
                .iter()
                .any(|term| term == "diagnostics")
        );

        assert_eq!(result.selected_memory.len(), 1);
        assert_eq!(result.selected_memory[0].memory_id, "mem-session-1");
        assert_eq!(result.selected_memory[0].session_id, session_id);

        assert_eq!(result.selected_shared_context.len(), 1);
        assert_eq!(result.selected_shared_context[0].item_id, "shared-exec-1");
        assert_eq!(result.selected_file_summaries.len(), 1);
        assert_eq!(
            result.selected_file_summaries[0].absolute_path,
            "/repo/src/parser.rs"
        );
        assert_eq!(result.selected_recent_turns.len(), 2);
        assert!(
            result
                .selected_recent_turns
                .iter()
                .any(|turn| turn.source == RecentTurnSource::Session
                    && turn.content == "Session turn about parser diagnostics")
        );
        assert!(
            result
                .selected_recent_turns
                .iter()
                .any(|turn| turn.source == RecentTurnSource::Project
                    && turn.content == "Project turn for parser fallback review")
        );
        assert_eq!(result.recent_turns_summary.resolved_count, 2);
        assert_eq!(result.recent_turns_summary.retained_count, 2);
    }

    #[test]
    fn knowledge_governed_output_excerpts_and_scoring() {
        let knowledge_store = KnowledgeStore::new();
        let long_content = "a]".repeat(60); // 120 chars, will be truncated to 96 + "…"

        knowledge_store.upsert(magi_knowledge_store::KnowledgeRecord {
            workspace_id: None,
            knowledge_id: "kb-adr-1".to_string(),
            kind: magi_knowledge_store::KnowledgeKind::Adr,
            title: "Architecture decision about caching".to_string(),
            content: long_content.clone(),
            tags: vec!["architecture".to_string(), "caching".to_string()],
            source_ref: Some("adr-001".to_string()),
            updated_at: UtcMillis(100),
        });
        knowledge_store.upsert(magi_knowledge_store::KnowledgeRecord {
            workspace_id: None,
            knowledge_id: "kb-faq-1".to_string(),
            kind: magi_knowledge_store::KnowledgeKind::Faq,
            title: "FAQ about caching strategies".to_string(),
            content: "Short content about caching".to_string(),
            tags: vec!["caching".to_string()],
            source_ref: None,
            updated_at: UtcMillis(200),
        });
        knowledge_store.upsert(magi_knowledge_store::KnowledgeRecord {
            workspace_id: None,
            knowledge_id: "kb-learning-1".to_string(),
            kind: magi_knowledge_store::KnowledgeKind::Learning,
            title: "Unrelated learning".to_string(),
            content: "This is about testing".to_string(),
            tags: vec!["testing".to_string()],
            source_ref: None,
            updated_at: UtcMillis(300),
        });

        let runtime = ContextRuntime::new(knowledge_store, MemoryStore::new());
        let budget = ContextBudget {
            max_turns: 0,
            max_knowledge: 10,
            max_memory: 0,
            max_shared_items: 0,
            max_file_summaries: 0,
        };
        let result = runtime.assemble(
            &budget,
            ContextAssemblyInput {
                recent_turns: vec![],
                knowledge_query: KnowledgeQuery {
                    workspace_id: None,
                    kind: None,
                    text: Some("caching".to_string()),
                    tags: vec![],
                    limit: 10,
                },
                memory_query: MemoryQuery {
                    session_id: SessionId::new("s"),
                    layer: None,
                    limit: 0,
                },
                shared_context: vec![],
                file_summaries: vec![],
            },
        );

        // "caching" matches kb-adr-1 (in title + tags) and kb-faq-1 (in title + content + tags)
        // kb-learning-1 should NOT match because it has no "caching" in indexed terms
        assert_eq!(result.selected_knowledge.len(), 2);
        let knowledge_ids: Vec<_> = result
            .selected_knowledge
            .iter()
            .map(|k| k.knowledge_id.as_str())
            .collect();
        assert!(knowledge_ids.contains(&"kb-adr-1"));
        assert!(knowledge_ids.contains(&"kb-faq-1"));

        // verify governed output excerpt truncation for the long-content record
        let adr = result
            .selected_knowledge
            .iter()
            .find(|k| k.knowledge_id == "kb-adr-1")
            .expect("adr record should be present");
        assert!(
            adr.excerpt.ends_with('…'),
            "long content should be truncated with ellipsis"
        );
        assert!(adr.excerpt.chars().count() <= 97); // 96 chars + "…"
        assert_eq!(adr.kind, magi_knowledge_store::KnowledgeKind::Adr);
        assert_eq!(adr.source_ref.as_deref(), Some("adr-001"));

        // verify matched_terms contain "caching"
        assert!(adr.matched_terms.contains(&"caching".to_string()));

        // scores should be > 0
        assert!(adr.score > 0);

        // the short-content FAQ should have full content in excerpt (no truncation)
        let faq = result
            .selected_knowledge
            .iter()
            .find(|k| k.knowledge_id == "kb-faq-1")
            .expect("faq record should be present");
        assert!(!faq.excerpt.ends_with('…'));
        assert_eq!(faq.excerpt, "Short content about caching");
    }

    #[test]
    fn code_index_governed_output_sidecars_flow_through_context_assembly() {
        let knowledge_store = KnowledgeStore::new();
        knowledge_store.ingest_code_index(magi_knowledge_store::CodeIndexIngestion {
            knowledge_id: "kb-code-1".to_string(),
            title: "Parser entrypoint".to_string(),
            content: "Parses repository manifests and emits governed summaries".to_string(),
            tags: vec!["parser".to_string(), "manifest".to_string()],
            source_ref: Some("src/parser.rs".to_string()),
            updated_at: UtcMillis(55),
            source: magi_knowledge_store::CodeIndexSource {
                path: "src/parser.rs".to_string(),
                language: Some("rust".to_string()),
                repo_ref: Some("magi-rust-rewrite".to_string()),
                commit_ref: Some("abc123".to_string()),
                start_line: Some(10),
                end_line: Some(48),
                symbol: Some(magi_knowledge_store::CodeIndexSymbol {
                    name: "parse_manifest".to_string(),
                    kind: magi_knowledge_store::CodeSymbolKind::Function,
                    container: Some("parser".to_string()),
                    signature: Some("fn parse_manifest(input: &str) -> Manifest".to_string()),
                }),
            },
            audit: Some(magi_knowledge_store::KnowledgeAuditLink {
                audit_event_id: "audit-knowledge-1".to_string(),
                trail_ref: Some("audit/trails/knowledge.json".to_string()),
                sequence: Some(7),
            }),
            governance: Some(magi_knowledge_store::KnowledgeGovernanceLink {
                outcome: magi_knowledge_store::KnowledgeGovernanceOutcome::Allowed,
                policy_refs: vec!["policy.knowledge.read".to_string()],
                rationale: Some("read-only code index".to_string()),
                audit_event_id: Some("audit-knowledge-1".to_string()),
            }),
        });

        let runtime = ContextRuntime::new(knowledge_store, MemoryStore::new());
        let budget = ContextBudget {
            max_turns: 0,
            max_knowledge: 5,
            max_memory: 0,
            max_shared_items: 0,
            max_file_summaries: 0,
        };
        let result = runtime.assemble(
            &budget,
            ContextAssemblyInput {
                recent_turns: vec![],
                knowledge_query: KnowledgeQuery {
                    workspace_id: None,
                    kind: Some(magi_knowledge_store::KnowledgeKind::CodeIndex),
                    text: Some("parse_manifest allowed".to_string()),
                    tags: vec!["parser".to_string()],
                    limit: 5,
                },
                memory_query: MemoryQuery {
                    session_id: SessionId::new("session-knowledge"),
                    layer: None,
                    limit: 0,
                },
                shared_context: vec![],
                file_summaries: vec![],
            },
        );

        assert_eq!(result.selected_knowledge.len(), 1);
        let selected = &result.selected_knowledge[0];
        assert_eq!(selected.knowledge_id, "kb-code-1");
        assert_eq!(
            selected.kind,
            magi_knowledge_store::KnowledgeKind::CodeIndex
        );
        assert_eq!(
            selected
                .code_source
                .as_ref()
                .map(|source| source.path.as_str()),
            Some("src/parser.rs")
        );
        assert_eq!(
            selected
                .code_source
                .as_ref()
                .and_then(|source| source.symbol.as_ref())
                .map(|symbol| symbol.name.as_str()),
            Some("parse_manifest")
        );
        assert_eq!(
            selected
                .audit_link
                .as_ref()
                .map(|audit| audit.audit_event_id.as_str()),
            Some("audit-knowledge-1")
        );
        assert_eq!(
            selected
                .governance_link
                .as_ref()
                .map(|governance| governance.policy_refs.as_slice()),
            Some(["policy.knowledge.read".to_string()].as_slice())
        );
        assert_eq!(
            selected
                .governance_link
                .as_ref()
                .map(|governance| governance.outcome.clone()),
            Some(magi_knowledge_store::KnowledgeGovernanceOutcome::Allowed)
        );
    }

    #[test]
    fn memory_layer_filtering_through_assembly() {
        let session_id = SessionId::new("session-1");
        let memory_store = MemoryStore::new();

        // 2 Recent, 2 Durable, 1 Shared
        memory_store.append(MemoryRecord {
            memory_id: "mem-recent-1".to_string(),
            session_id: session_id.clone(),
            layer: magi_memory_store::MemoryLayer::Recent,
            content: "recent memory 1".to_string(),
            provenance: None,
            compacted: false,
            created_at: UtcMillis(10),
        });
        memory_store.append(MemoryRecord {
            memory_id: "mem-recent-2".to_string(),
            session_id: session_id.clone(),
            layer: magi_memory_store::MemoryLayer::Recent,
            content: "recent memory 2".to_string(),
            provenance: None,
            compacted: false,
            created_at: UtcMillis(20),
        });
        memory_store.append(MemoryRecord {
            memory_id: "mem-durable-1".to_string(),
            session_id: session_id.clone(),
            layer: magi_memory_store::MemoryLayer::Durable,
            content: "durable memory 1".to_string(),
            provenance: None,
            compacted: false,
            created_at: UtcMillis(30),
        });
        memory_store.append(MemoryRecord {
            memory_id: "mem-durable-2".to_string(),
            session_id: session_id.clone(),
            layer: magi_memory_store::MemoryLayer::Durable,
            content: "durable memory 2".to_string(),
            provenance: None,
            compacted: false,
            created_at: UtcMillis(40),
        });
        memory_store.append(MemoryRecord {
            memory_id: "mem-shared-1".to_string(),
            session_id: session_id.clone(),
            layer: magi_memory_store::MemoryLayer::Shared,
            content: "shared memory 1".to_string(),
            provenance: None,
            compacted: false,
            created_at: UtcMillis(50),
        });

        let runtime = ContextRuntime::new(KnowledgeStore::new(), memory_store);

        // query only Durable layer, budget allows 1
        let budget = ContextBudget {
            max_turns: 0,
            max_knowledge: 0,
            max_memory: 1,
            max_shared_items: 0,
            max_file_summaries: 0,
        };
        let result = runtime.assemble(
            &budget,
            ContextAssemblyInput {
                recent_turns: vec![],
                knowledge_query: KnowledgeQuery {
                    workspace_id: None,
                    kind: None,
                    text: None,
                    tags: vec![],
                    limit: 0,
                },
                memory_query: MemoryQuery {
                    session_id: session_id.clone(),
                    layer: Some(magi_memory_store::MemoryLayer::Durable),
                    limit: 10,
                },
                shared_context: vec![],
                file_summaries: vec![],
            },
        );

        // 2 durable records exist, budget allows 1 → truncated；按"最新优先"语义保留较新的一条
        assert_eq!(result.selected_memory.len(), 1);
        assert_eq!(result.selected_memory[0].memory_id, "mem-durable-2");
        assert_eq!(
            result.selected_memory[0].layer,
            magi_memory_store::MemoryLayer::Durable
        );
        assert_eq!(result.usage.used_memory, 1);

        // truncation record for memory
        let memory_truncation = result
            .usage
            .truncations
            .iter()
            .find(|t| t.part == "memory")
            .expect("memory truncation record should exist");
        assert_eq!(memory_truncation.original_count, 2);
        assert_eq!(memory_truncation.retained_count, 1);
    }

    #[test]
    fn extracted_memory_records_flow_through_context_assembly() {
        let session_id = SessionId::new("session-extract");
        let memory_store = MemoryStore::new();
        let linkage =
            memory_store.apply_extraction(magi_memory_store::MemoryExtractionApplyRequest {
                extraction_id: "extract-ctx-1".to_string(),
                session_id: session_id.clone(),
                source_ref: Some("timeline:42".to_string()),
                summary: "Summarized stable user preferences".to_string(),
                memories: vec![
                    magi_memory_store::ExtractedMemory {
                        memory_id: "mem-extract-2".to_string(),
                        layer: magi_memory_store::MemoryLayer::Durable,
                        content: "User prefers deterministic output".to_string(),
                        created_at: UtcMillis(40),
                    },
                    magi_memory_store::ExtractedMemory {
                        memory_id: "mem-extract-1".to_string(),
                        layer: magi_memory_store::MemoryLayer::Durable,
                        content: "Prefer showing audit links in context".to_string(),
                        created_at: UtcMillis(30),
                    },
                ],
                created_at: UtcMillis(25),
            });
        assert_eq!(linkage.produced_records.len(), 2);
        assert!(
            memory_store
                .verify_extraction_linkage("extract-ctx-1")
                .is_some_and(|verification| verification.is_consistent)
        );

        let runtime = ContextRuntime::new(KnowledgeStore::new(), memory_store);
        let budget = ContextBudget {
            max_turns: 0,
            max_knowledge: 0,
            max_memory: 5,
            max_shared_items: 0,
            max_file_summaries: 0,
        };
        let result = runtime.assemble(
            &budget,
            ContextAssemblyInput {
                recent_turns: vec![],
                knowledge_query: KnowledgeQuery {
                    workspace_id: None,
                    kind: None,
                    text: None,
                    tags: vec![],
                    limit: 0,
                },
                memory_query: MemoryQuery {
                    session_id: session_id.clone(),
                    layer: Some(magi_memory_store::MemoryLayer::Durable),
                    limit: 5,
                },
                shared_context: vec![],
                file_summaries: vec![],
            },
        );

        assert_eq!(result.selected_memory.len(), 2);
        assert_eq!(result.selected_memory[0].memory_id, "mem-extract-1");
        assert_eq!(result.selected_memory[1].memory_id, "mem-extract-2");
        for record in &result.selected_memory {
            assert_eq!(record.session_id, session_id);
            assert_eq!(
                record
                    .provenance
                    .as_ref()
                    .map(|provenance| provenance.source.as_str()),
                Some("extraction")
            );
            assert_eq!(
                record
                    .provenance
                    .as_ref()
                    .and_then(|provenance| provenance.extracted_from.as_deref()),
                Some("extract-ctx-1")
            );
        }
    }

    #[test]
    fn session_and_project_turns_deduplication_prefers_session() {
        let session_id = SessionId::new("session-1");
        let project_key = "project-a".to_string();
        let ts = |v: u64| UtcMillis(v);

        // session timeline: 2 entries, one with content "shared turn"
        let session_store = SessionStore::from_state(SessionStoreState {
            current_session_id: Some(session_id.clone()),
            sessions: vec![SessionRecord {
                session_id: session_id.clone(),
                title: "Session".to_string(),
                status: magi_core::SessionLifecycleStatus::Active,
                created_at: ts(1),
                updated_at: ts(1),
                message_count: None,
                workspace_id: None,
            }],
            timeline: vec![
                TimelineEntry {
                    entry_id: "timeline-1".to_string(),
                    session_id: session_id.clone(),
                    kind: TimelineEntryKind::SystemNote,
                    message: "unique session turn".to_string(),
                    occurred_at: ts(100),
                },
                TimelineEntry {
                    entry_id: "timeline-2".to_string(),
                    session_id: session_id.clone(),
                    kind: TimelineEntryKind::SystemNote,
                    message: "shared turn".to_string(),
                    occurred_at: ts(200),
                },
            ],
            notifications: vec![],
            canonical_turns: vec![],
            thread_registry: vec![],
            execution_sidecar_store: SessionExecutionSidecarStoreState {
                runtime_sidecars: vec![],
            },
        });

        // project recent turns: 2 entries, one with same content "shared turn"
        let project_recent_turn_store = ProjectRecentTurnStore::default();
        project_recent_turn_store.upsert(ProjectRecentTurnRecord {
            turn_id: "project-turn-1".to_string(),
            project_key: project_key.clone(),
            content: "shared turn".to_string(),
            updated_at: ts(150),
        });
        project_recent_turn_store.upsert(ProjectRecentTurnRecord {
            turn_id: "project-turn-2".to_string(),
            project_key: project_key.clone(),
            content: "unique project turn".to_string(),
            updated_at: ts(250),
        });

        let runtime = ContextRuntime::with_runtime_sources(
            KnowledgeStore::new(),
            MemoryStore::new(),
            session_store,
            SharedContextPool::default(),
            FileSummaryStore::default(),
            project_recent_turn_store,
        );

        // deduplicate = true, generous budget
        let budget = ContextBudget {
            max_turns: 10,
            max_knowledge: 0,
            max_memory: 0,
            max_shared_items: 0,
            max_file_summaries: 0,
        };
        let result = runtime.assemble_from_runtime_sources(
            &budget,
            ContextSourceAssemblyInput {
                recent_turns_query: RecentTurnsSourceQuery {
                    session_id: Some(session_id.clone()),
                    project_key: Some(project_key.clone()),
                    limit: 10,
                    max_session_turns: None,
                    max_project_turns: None,
                    deduplicate: true,
                },
                knowledge_query: KnowledgeQuery {
                    workspace_id: None,
                    kind: None,
                    text: None,
                    tags: vec![],
                    limit: 0,
                },
                memory_query: MemoryQuery {
                    session_id: session_id.clone(),
                    layer: None,
                    limit: 0,
                },
                shared_context_query: SharedContextQuery {
                    session_id: None,
                    project_key: None,
                    limit: 0,
                },
                file_summary_query: FileSummaryQuery {
                    workspace_id: None,
                    project_key: None,
                    path_prefix: None,
                    limit: 0,
                },
            },
        );

        // before dedup: 4 records (2 session + 2 project)
        // "shared turn" appears twice → dedup keeps the later one (Session source at ts 200)
        // after dedup: 3 unique turns
        assert_eq!(result.selected_recent_turns.len(), 3);
        assert_eq!(result.recent_turns_summary.resolved_count, 3);
        assert_eq!(result.recent_turns_summary.retained_count, 3);

        // verify the "shared turn" retained is from Session source (higher priority)
        let shared_turn = result
            .selected_recent_turns
            .iter()
            .find(|t| t.content == "shared turn")
            .expect("shared turn should be retained");
        assert_eq!(shared_turn.source, RecentTurnSource::Session);

        // verify all 3 unique contents are present
        let contents: Vec<_> = result
            .selected_recent_turns
            .iter()
            .map(|t| t.content.as_str())
            .collect();
        assert!(contents.contains(&"unique session turn"));
        assert!(contents.contains(&"shared turn"));
        assert!(contents.contains(&"unique project turn"));
    }

    #[test]
    fn shared_context_and_file_summary_query_filtering() {
        let session_id = SessionId::new("session-1");
        let other_session_id = SessionId::new("session-other");
        let workspace_id = WorkspaceId::new("workspace-1");
        let project_key = "project-a".to_string();
        let ts = |v: u64| UtcMillis(v);

        let shared_context_pool = SharedContextPool::default();
        // 2 items for session-1, 1 item for session-other
        shared_context_pool.upsert(SharedContextRecord {
            item: SharedContextItem {
                item_id: "shared-1".to_string(),
                title: "Context A".to_string(),
                content: "content a".to_string(),
            },
            session_id: Some(session_id.clone()),
            project_key: Some(project_key.clone()),
            updated_at: ts(100),
        });
        shared_context_pool.upsert(SharedContextRecord {
            item: SharedContextItem {
                item_id: "shared-2".to_string(),
                title: "Context B".to_string(),
                content: "content b".to_string(),
            },
            session_id: Some(session_id.clone()),
            project_key: Some(project_key.clone()),
            updated_at: ts(200),
        });
        shared_context_pool.upsert(SharedContextRecord {
            item: SharedContextItem {
                item_id: "shared-3".to_string(),
                title: "Other session context".to_string(),
                content: "other content".to_string(),
            },
            session_id: Some(other_session_id.clone()),
            project_key: Some(project_key.clone()),
            updated_at: ts(300),
        });

        let file_summary_store = FileSummaryStore::default();
        // 2 files under /src/, 1 under /test/
        file_summary_store.upsert(FileSummaryRecord {
            item: FileSummaryItem {
                absolute_path: "/src/main.rs".to_string(),
                summary: "Main entry point".to_string(),
            },
            workspace_id: Some(workspace_id.clone()),
            project_key: Some(project_key.clone()),
            updated_at: ts(100),
        });
        file_summary_store.upsert(FileSummaryRecord {
            item: FileSummaryItem {
                absolute_path: "/src/lib.rs".to_string(),
                summary: "Library root".to_string(),
            },
            workspace_id: Some(workspace_id.clone()),
            project_key: Some(project_key.clone()),
            updated_at: ts(200),
        });
        file_summary_store.upsert(FileSummaryRecord {
            item: FileSummaryItem {
                absolute_path: "/test/integration.rs".to_string(),
                summary: "Integration tests".to_string(),
            },
            workspace_id: Some(workspace_id.clone()),
            project_key: Some(project_key.clone()),
            updated_at: ts(300),
        });

        let runtime = ContextRuntime::with_runtime_sources(
            KnowledgeStore::new(),
            MemoryStore::new(),
            SessionStore::from_state(SessionStoreState {
                current_session_id: None,
                sessions: vec![],
                timeline: vec![],
                notifications: vec![],
                canonical_turns: vec![],
                thread_registry: vec![],
                execution_sidecar_store: SessionExecutionSidecarStoreState {
                    runtime_sidecars: vec![],
                },
            }),
            shared_context_pool,
            file_summary_store,
            ProjectRecentTurnStore::default(),
        );

        let budget = ContextBudget {
            max_turns: 0,
            max_knowledge: 0,
            max_memory: 0,
            max_shared_items: 10,
            max_file_summaries: 10,
        };

        let result = runtime.assemble_from_runtime_sources(
            &budget,
            ContextSourceAssemblyInput {
                recent_turns_query: RecentTurnsSourceQuery {
                    session_id: None,
                    project_key: None,
                    limit: 0,
                    max_session_turns: None,
                    max_project_turns: None,
                    deduplicate: false,
                },
                knowledge_query: KnowledgeQuery {
                    workspace_id: None,
                    kind: None,
                    text: None,
                    tags: vec![],
                    limit: 0,
                },
                memory_query: MemoryQuery {
                    session_id: SessionId::new("s"),
                    layer: None,
                    limit: 0,
                },
                shared_context_query: SharedContextQuery {
                    session_id: Some(session_id.clone()),
                    project_key: Some(project_key.clone()),
                    limit: 10,
                },
                file_summary_query: FileSummaryQuery {
                    workspace_id: Some(workspace_id.clone()),
                    project_key: Some(project_key.clone()),
                    path_prefix: Some("/src/".to_string()),
                    limit: 10,
                },
            },
        );

        // shared context: only 2 items for session-1 (not the other session)
        assert_eq!(result.selected_shared_context.len(), 2);
        let shared_ids: Vec<_> = result
            .selected_shared_context
            .iter()
            .map(|s| s.item_id.as_str())
            .collect();
        assert!(shared_ids.contains(&"shared-1"));
        assert!(shared_ids.contains(&"shared-2"));

        // file summaries: only 2 files under /src/ prefix
        assert_eq!(result.selected_file_summaries.len(), 2);
        let file_paths: Vec<_> = result
            .selected_file_summaries
            .iter()
            .map(|f| f.absolute_path.as_str())
            .collect();
        assert!(file_paths.contains(&"/src/main.rs"));
        assert!(file_paths.contains(&"/src/lib.rs"));

        // no truncations because we're within budget
        assert!(result.usage.truncations.is_empty());
    }

    #[test]
    fn context_scope_queries_keep_workspace_file_summaries_as_hard_boundary() {
        let session_id = SessionId::new("session-scope");
        let other_session_id = SessionId::new("session-scope-other");
        let workspace_id = WorkspaceId::new("workspace-scope");
        let other_workspace_id = WorkspaceId::new("workspace-scope-other");
        let project_key = "project-scope".to_string();
        let other_project_key = "project-scope-other".to_string();

        let shared_context_pool = SharedContextPool::default();
        for (item_id, session_id, project_key, updated_at) in [
            (
                "shared-session",
                Some(session_id.clone()),
                None,
                UtcMillis(10),
            ),
            (
                "shared-project",
                None,
                Some(project_key.clone()),
                UtcMillis(20),
            ),
            (
                "shared-both",
                Some(session_id.clone()),
                Some(project_key.clone()),
                UtcMillis(30),
            ),
            (
                "shared-other-session",
                Some(other_session_id),
                Some(project_key.clone()),
                UtcMillis(40),
            ),
            (
                "shared-other-project",
                None,
                Some(other_project_key.clone()),
                UtcMillis(50),
            ),
        ] {
            shared_context_pool.upsert(SharedContextRecord {
                item: SharedContextItem {
                    item_id: item_id.to_string(),
                    title: item_id.to_string(),
                    content: item_id.to_string(),
                },
                session_id,
                project_key,
                updated_at,
            });
        }

        let file_summary_store = FileSummaryStore::default();
        for (path, workspace_id, project_key, updated_at) in [
            (
                "/repo/workspace.rs",
                Some(workspace_id.clone()),
                None,
                UtcMillis(10),
            ),
            (
                "/repo/project.rs",
                None,
                Some(project_key.clone()),
                UtcMillis(20),
            ),
            (
                "/repo/both.rs",
                Some(workspace_id.clone()),
                Some(project_key.clone()),
                UtcMillis(30),
            ),
            (
                "/repo/other-workspace.rs",
                Some(other_workspace_id),
                Some(project_key.clone()),
                UtcMillis(40),
            ),
            (
                "/repo/other-project.rs",
                Some(workspace_id.clone()),
                Some(other_project_key),
                UtcMillis(50),
            ),
        ] {
            file_summary_store.upsert(FileSummaryRecord {
                item: FileSummaryItem {
                    absolute_path: path.to_string(),
                    summary: path.to_string(),
                },
                workspace_id,
                project_key,
                updated_at,
            });
        }

        let shared = shared_context_pool.query(&SharedContextQuery {
            session_id: Some(session_id),
            project_key: Some(project_key.clone()),
            limit: 10,
        });
        let shared_ids = shared
            .iter()
            .map(|record| record.item.item_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            shared_ids,
            vec!["shared-both", "shared-project", "shared-session"]
        );

        let files = file_summary_store.query(&FileSummaryQuery {
            workspace_id: Some(workspace_id),
            project_key: Some(project_key),
            path_prefix: None,
            limit: 10,
        });
        let file_paths = files
            .iter()
            .map(|record| record.item.absolute_path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(file_paths, vec!["/repo/both.rs", "/repo/workspace.rs"]);
    }

    #[test]
    fn project_only_file_summary_query_excludes_workspace_bound_records() {
        let project_key = "project-file-summary-only".to_string();
        let workspace_id = WorkspaceId::new("workspace-file-summary-only");
        let file_summary_store = FileSummaryStore::default();

        for (path, workspace_id, updated_at) in [
            ("/repo/project-only.rs", None, UtcMillis(20)),
            (
                "/repo/workspace-bound.rs",
                Some(workspace_id.clone()),
                UtcMillis(30),
            ),
        ] {
            file_summary_store.upsert(FileSummaryRecord {
                item: FileSummaryItem {
                    absolute_path: path.to_string(),
                    summary: path.to_string(),
                },
                workspace_id,
                project_key: Some(project_key.clone()),
                updated_at,
            });
        }

        let files = file_summary_store.query(&FileSummaryQuery {
            workspace_id: None,
            project_key: Some(project_key),
            path_prefix: None,
            limit: 10,
        });
        let file_paths = files
            .iter()
            .map(|record| record.item.absolute_path.as_str())
            .collect::<Vec<_>>();

        assert_eq!(file_paths, vec!["/repo/project-only.rs"]);
    }

    #[test]
    fn runtime_source_stores_keep_same_identity_separate_across_scopes() {
        let session_a = SessionId::new("session-scope-key-a");
        let session_b = SessionId::new("session-scope-key-b");
        let workspace_a = WorkspaceId::new("workspace-scope-key-a");
        let workspace_b = WorkspaceId::new("workspace-scope-key-b");
        let project_a = "project-scope-key-a".to_string();
        let project_b = "project-scope-key-b".to_string();

        let shared_context_pool = SharedContextPool::default();
        for (session_id, content) in [
            (session_a.clone(), "session a context"),
            (session_b.clone(), "session b context"),
        ] {
            shared_context_pool.upsert(SharedContextRecord {
                item: SharedContextItem {
                    item_id: "shared-same-id".to_string(),
                    title: "same shared id".to_string(),
                    content: content.to_string(),
                },
                session_id: Some(session_id),
                project_key: Some(project_a.clone()),
                updated_at: UtcMillis(10),
            });
        }

        let session_a_shared = shared_context_pool.query(&SharedContextQuery {
            session_id: Some(session_a),
            project_key: Some(project_a.clone()),
            limit: 10,
        });
        let session_b_shared = shared_context_pool.query(&SharedContextQuery {
            session_id: Some(session_b),
            project_key: Some(project_a.clone()),
            limit: 10,
        });
        assert_eq!(session_a_shared.len(), 1);
        assert_eq!(session_b_shared.len(), 1);
        assert_eq!(session_a_shared[0].item.content, "session a context");
        assert_eq!(session_b_shared[0].item.content, "session b context");

        let file_summary_store = FileSummaryStore::default();
        for (workspace_id, project_key, summary) in [
            (
                workspace_a.clone(),
                project_a.clone(),
                "workspace a summary",
            ),
            (
                workspace_b.clone(),
                project_a.clone(),
                "workspace b summary",
            ),
            (workspace_a.clone(), project_b.clone(), "project b summary"),
        ] {
            file_summary_store.upsert(FileSummaryRecord {
                item: FileSummaryItem {
                    absolute_path: "/repo/src/lib.rs".to_string(),
                    summary: summary.to_string(),
                },
                workspace_id: Some(workspace_id),
                project_key: Some(project_key),
                updated_at: UtcMillis(10),
            });
        }

        let workspace_a_project_a_files = file_summary_store.query(&FileSummaryQuery {
            workspace_id: Some(workspace_a),
            project_key: Some(project_a.clone()),
            path_prefix: None,
            limit: 10,
        });
        let workspace_b_project_a_files = file_summary_store.query(&FileSummaryQuery {
            workspace_id: Some(workspace_b),
            project_key: Some(project_a),
            path_prefix: None,
            limit: 10,
        });
        assert_eq!(workspace_a_project_a_files.len(), 1);
        assert_eq!(workspace_b_project_a_files.len(), 1);
        assert_eq!(
            workspace_a_project_a_files[0].item.summary,
            "workspace a summary"
        );
        assert_eq!(
            workspace_b_project_a_files[0].item.summary,
            "workspace b summary"
        );

        let project_recent_turn_store = ProjectRecentTurnStore::default();
        for (project_key, content) in [
            ("project-recent-a".to_string(), "project a turn"),
            ("project-recent-b".to_string(), "project b turn"),
        ] {
            project_recent_turn_store.upsert(ProjectRecentTurnRecord {
                turn_id: "turn-same-id".to_string(),
                project_key,
                content: content.to_string(),
                updated_at: UtcMillis(10),
            });
        }
        assert_eq!(
            project_recent_turn_store.query(&ProjectRecentTurnsQuery {
                project_key: "project-recent-a".to_string(),
                limit: 10,
            })[0]
                .content,
            "project a turn"
        );
        assert_eq!(
            project_recent_turn_store.query(&ProjectRecentTurnsQuery {
                project_key: "project-recent-b".to_string(),
                limit: 10,
            })[0]
                .content,
            "project b turn"
        );
    }

    #[test]
    fn zero_budget_produces_empty_result_with_no_truncations() {
        let session_id = SessionId::new("session-1");
        let knowledge_store = KnowledgeStore::new();
        knowledge_store.upsert(magi_knowledge_store::KnowledgeRecord {
            workspace_id: None,
            knowledge_id: "kb-1".to_string(),
            kind: magi_knowledge_store::KnowledgeKind::Faq,
            title: "Some knowledge".to_string(),
            content: "content".to_string(),
            tags: vec![],
            source_ref: None,
            updated_at: UtcMillis(1),
        });
        let memory_store = MemoryStore::new();
        memory_store.append(MemoryRecord {
            memory_id: "mem-1".to_string(),
            session_id: session_id.clone(),
            layer: magi_memory_store::MemoryLayer::Recent,
            content: "memory".to_string(),
            provenance: None,
            compacted: false,
            created_at: UtcMillis(1),
        });

        let runtime = ContextRuntime::new(knowledge_store, memory_store);
        let budget = ContextBudget {
            max_turns: 0,
            max_knowledge: 0,
            max_memory: 0,
            max_shared_items: 0,
            max_file_summaries: 0,
        };

        let result = runtime.assemble(
            &budget,
            ContextAssemblyInput {
                recent_turns: vec!["a turn".to_string()],
                knowledge_query: KnowledgeQuery {
                    workspace_id: None,
                    kind: None,
                    text: None,
                    tags: vec![],
                    limit: 0,
                },
                memory_query: MemoryQuery {
                    session_id: session_id.clone(),
                    layer: None,
                    limit: 0,
                },
                shared_context: vec![SharedContextItem {
                    item_id: "s-1".to_string(),
                    title: "Shared".to_string(),
                    content: "shared".to_string(),
                }],
                file_summaries: vec![FileSummaryItem {
                    absolute_path: "/test.rs".to_string(),
                    summary: "test".to_string(),
                }],
            },
        );

        assert_eq!(result.selected_turns.len(), 0);
        assert_eq!(result.selected_knowledge.len(), 0);
        assert_eq!(result.selected_memory.len(), 0);
        assert_eq!(result.selected_shared_context.len(), 0);
        assert_eq!(result.selected_file_summaries.len(), 0);
        assert_eq!(result.usage.used_turns, 0);
        assert_eq!(result.usage.used_knowledge, 0);
        assert_eq!(result.usage.used_memory, 0);
        assert_eq!(result.usage.used_shared_items, 0);
        assert_eq!(result.usage.used_file_summaries, 0);

        // turns have 1 provided item truncated to 0, shared_context has 1→0, file_summaries has 1→0
        // knowledge limit=0 so no query happens (0 matches), memory limit=0 → 0 results
        let turns_trunc = result
            .usage
            .truncations
            .iter()
            .find(|t| t.part == "recent_turns");
        assert!(turns_trunc.is_some());
        assert_eq!(turns_trunc.unwrap().original_count, 1);
        assert_eq!(turns_trunc.unwrap().retained_count, 0);

        let shared_trunc = result
            .usage
            .truncations
            .iter()
            .find(|t| t.part == "shared_context");
        assert!(shared_trunc.is_some());
        assert_eq!(shared_trunc.unwrap().original_count, 1);
        assert_eq!(shared_trunc.unwrap().retained_count, 0);

        let file_trunc = result
            .usage
            .truncations
            .iter()
            .find(|t| t.part == "file_summaries");
        assert!(file_trunc.is_some());
        assert_eq!(file_trunc.unwrap().original_count, 1);
        assert_eq!(file_trunc.unwrap().retained_count, 0);
    }
}
