mod execution_writeback;
pub mod task_store;
pub mod task_worker_catalog;

use magi_context_runtime::ContextAssemblyResult;
use magi_worker_runtime::WorkerRuntime;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub use execution_writeback::{DispatchMemoryExtractionInput, ExecutionWritebackPlans};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExecutionContextSummary {
    pub used_turns: usize,
    pub used_knowledge: usize,
    pub used_memory: usize,
    pub used_shared_items: usize,
    pub used_file_summaries: usize,
    #[serde(default)]
    pub recent_turn_resolved_count: usize,
    #[serde(default)]
    pub recent_turn_retained_count: usize,
    #[serde(default)]
    pub recent_turn_session_source_count: usize,
    #[serde(default)]
    pub recent_turn_project_source_count: usize,
    #[serde(default)]
    pub recent_turn_provided_source_count: usize,
    pub truncation_count: usize,
    pub truncation_parts: Vec<String>,
    pub knowledge_ids: Vec<String>,
    pub knowledge_source_paths: Vec<String>,
    pub memory_ids: Vec<String>,
    pub memory_extraction_refs: Vec<String>,
    #[serde(default)]
    pub shared_context_ids: Vec<String>,
    #[serde(default)]
    pub file_summary_paths: Vec<String>,
    pub code_index_knowledge_count: usize,
    pub audited_knowledge_count: usize,
    pub governed_knowledge_count: usize,
    pub extracted_memory_count: usize,
    pub provenance_linked_memory_count: usize,
}

impl ExecutionContextSummary {
    pub fn from_context_assembly(result: &ContextAssemblyResult) -> Self {
        let mut truncation_parts = result
            .usage
            .truncations
            .iter()
            .map(|record| record.part.clone())
            .collect::<Vec<_>>();
        truncation_parts.sort();
        truncation_parts.dedup();

        let mut knowledge_ids = result
            .selected_knowledge
            .iter()
            .map(|record| record.knowledge_id.clone())
            .collect::<Vec<_>>();
        knowledge_ids.sort();
        knowledge_ids.dedup();

        let mut knowledge_source_paths = result
            .selected_knowledge
            .iter()
            .filter_map(|record| {
                record
                    .code_source
                    .as_ref()
                    .map(|source| source.path.clone())
            })
            .collect::<Vec<_>>();
        knowledge_source_paths.sort();
        knowledge_source_paths.dedup();

        let mut memory_ids = result
            .selected_memory
            .iter()
            .map(|record| record.memory_id.clone())
            .collect::<Vec<_>>();
        memory_ids.sort();
        memory_ids.dedup();

        let mut memory_extraction_refs = result
            .selected_memory
            .iter()
            .filter_map(|record| {
                record
                    .provenance
                    .as_ref()
                    .and_then(|provenance| provenance.extracted_from.clone())
            })
            .collect::<Vec<_>>();
        memory_extraction_refs.sort();
        memory_extraction_refs.dedup();

        let mut shared_context_ids = result
            .selected_shared_context
            .iter()
            .map(|record| record.item_id.clone())
            .collect::<Vec<_>>();
        shared_context_ids.sort();
        shared_context_ids.dedup();

        let mut file_summary_paths = result
            .selected_file_summaries
            .iter()
            .map(|record| record.absolute_path.clone())
            .collect::<Vec<_>>();
        file_summary_paths.sort();
        file_summary_paths.dedup();

        Self {
            used_turns: result.usage.used_turns,
            used_knowledge: result.usage.used_knowledge,
            used_memory: result.usage.used_memory,
            used_shared_items: result.usage.used_shared_items,
            used_file_summaries: result.usage.used_file_summaries,
            recent_turn_resolved_count: result.recent_turns_summary.resolved_count,
            recent_turn_retained_count: result.recent_turns_summary.retained_count,
            recent_turn_session_source_count: result.recent_turns_summary.session_source_count,
            recent_turn_project_source_count: result.recent_turns_summary.project_source_count,
            recent_turn_provided_source_count: result.recent_turns_summary.provided_source_count,
            truncation_count: result.usage.truncations.len(),
            truncation_parts,
            knowledge_ids,
            knowledge_source_paths,
            memory_ids,
            memory_extraction_refs,
            shared_context_ids,
            file_summary_paths,
            code_index_knowledge_count: result
                .selected_knowledge
                .iter()
                .filter(|record| record.code_source.is_some())
                .count(),
            audited_knowledge_count: result
                .selected_knowledge
                .iter()
                .filter(|record| record.audit_link.is_some())
                .count(),
            governed_knowledge_count: result
                .selected_knowledge
                .iter()
                .filter(|record| record.governance_link.is_some())
                .count(),
            extracted_memory_count: result
                .selected_memory
                .iter()
                .filter(|record| {
                    record.provenance.as_ref().is_some_and(|provenance| {
                        provenance.source.eq_ignore_ascii_case("extraction")
                            || provenance.extracted_from.is_some()
                    })
                })
                .count(),
            provenance_linked_memory_count: result
                .selected_memory
                .iter()
                .filter(|record| record.provenance.is_some())
                .count(),
        }
    }
}

/// 执行管线持有的运行时句柄：任务事实由 `TaskStore` 拥有，分支检查点由 `WorkerRuntime` 拥有。
#[derive(Clone)]
pub struct OrchestratedExecutionRuntime {
    task_store: Arc<task_store::TaskStore>,
    worker_runtime: WorkerRuntime,
}

impl OrchestratedExecutionRuntime {
    pub fn new(worker_runtime: WorkerRuntime) -> Self {
        Self {
            task_store: Arc::new(task_store::TaskStore::new()),
            worker_runtime,
        }
    }

    pub fn with_task_store(mut self, task_store: Arc<task_store::TaskStore>) -> Self {
        self.task_store = task_store;
        self
    }

    pub fn worker_runtime(&self) -> &WorkerRuntime {
        &self.worker_runtime
    }

    pub fn task_store(&self) -> &task_store::TaskStore {
        &self.task_store
    }
}

#[cfg(test)]
mod tests;
