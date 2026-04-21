use crate::{
    ContextAssemblyResult, ContextBudget, ContextRuntime, ContextSourceAssemblyInput,
    FileSummaryQuery, RecentTurnsSourceQuery, SharedContextQuery,
};
use magi_core::{SessionId, WorkspaceId};
use magi_knowledge_store::KnowledgeQuery;
use magi_memory_store::MemoryQuery;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExecutionContextClues {
    pub mission: Option<String>,
    pub assignment: Option<String>,
    pub todo: Option<String>,
}

impl ExecutionContextClues {
    pub fn knowledge_query_text(&self) -> Option<String> {
        let parts = [
            self.mission.as_deref(),
            self.assignment.as_deref(),
            self.todo.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionContextAssemblyRequest {
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub project_key: Option<String>,
    pub clues: ExecutionContextClues,
    pub budget: ContextBudget,
}

impl ExecutionContextAssemblyRequest {
    pub fn knowledge_query(&self) -> KnowledgeQuery {
        KnowledgeQuery {
            kind: None,
            text: self.clues.knowledge_query_text(),
            tags: vec![],
            limit: self.budget.max_knowledge,
        }
    }

    pub fn memory_query(&self) -> MemoryQuery {
        MemoryQuery {
            session_id: self.session_id.clone(),
            layer: None,
            limit: self.budget.max_memory,
        }
    }

    pub fn to_source_assembly_input(&self) -> ContextSourceAssemblyInput {
        ContextSourceAssemblyInput {
            recent_turns_query: RecentTurnsSourceQuery {
                session_id: Some(self.session_id.clone()),
                project_key: self.project_key.clone(),
                limit: self.budget.max_turns,
                max_session_turns: Some(self.budget.max_turns),
                max_project_turns: Some(self.budget.max_turns),
                deduplicate: true,
            },
            knowledge_query: self.knowledge_query(),
            memory_query: self.memory_query(),
            shared_context_query: SharedContextQuery {
                session_id: Some(self.session_id.clone()),
                project_key: self.project_key.clone(),
                limit: self.budget.max_shared_items,
            },
            file_summary_query: FileSummaryQuery {
                workspace_id: Some(self.workspace_id.clone()),
                project_key: self.project_key.clone(),
                path_prefix: None,
                limit: self.budget.max_file_summaries,
            },
        }
    }
}

impl ContextRuntime {
    pub fn assemble_execution_context(
        &self,
        request: &ExecutionContextAssemblyRequest,
    ) -> ContextAssemblyResult {
        self.assemble_from_runtime_sources(&request.budget, request.to_source_assembly_input())
    }
}
