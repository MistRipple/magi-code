use super::{ContextAssemblyResult, ContextBudgetUsage};
use crate::budgeting::BudgetedContextSelection;

pub(super) fn build_context_assembly_result(
    selection: BudgetedContextSelection,
) -> ContextAssemblyResult {
    ContextAssemblyResult {
        usage: ContextBudgetUsage {
            used_turns: selection.selected_turns.len(),
            used_knowledge: selection.selected_knowledge.len(),
            used_memory: selection.selected_memory.len(),
            used_shared_items: selection.selected_shared_context.len(),
            used_file_summaries: selection.selected_file_summaries.len(),
            truncations: selection.truncations,
        },
        selected_recent_turns: selection.selected_recent_turns,
        selected_turns: selection.selected_turns,
        selected_knowledge: selection.selected_knowledge,
        selected_memory: selection.selected_memory,
        selected_shared_context: selection.selected_shared_context,
        selected_file_summaries: selection.selected_file_summaries,
        recent_turns_summary: selection.recent_turns_summary,
    }
}
