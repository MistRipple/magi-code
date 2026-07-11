use super::{
    ContextBudget, ContextRuntime, FileSummaryItem, RecentTurnRecord, RecentTurnSource,
    RecentTurnsResolutionSummary, SharedContextItem, TruncationRecord,
};
use magi_knowledge_store::{GovernedKnowledgeOutput, KnowledgeQuery};
use magi_memory_store::{MemoryQuery, MemoryRecord};

pub(crate) struct BudgetedContextSelection {
    pub selected_recent_turns: Vec<RecentTurnRecord>,
    pub selected_turns: Vec<String>,
    pub selected_knowledge: Vec<GovernedKnowledgeOutput>,
    pub selected_memory: Vec<MemoryRecord>,
    pub selected_shared_context: Vec<SharedContextItem>,
    pub selected_file_summaries: Vec<FileSummaryItem>,
    pub recent_turns_summary: RecentTurnsResolutionSummary,
    pub truncations: Vec<TruncationRecord>,
}

pub(super) struct ContextSelectionInput {
    pub recent_turns: Vec<RecentTurnRecord>,
    pub recent_turns_summary: RecentTurnsResolutionSummary,
    pub knowledge_query: KnowledgeQuery,
    pub memory_query: MemoryQuery,
    pub shared_context: Vec<SharedContextItem>,
    pub file_summaries: Vec<FileSummaryItem>,
}

pub(super) fn assemble_budgeted_selection(
    runtime: &ContextRuntime,
    budget: &ContextBudget,
    input: ContextSelectionInput,
) -> BudgetedContextSelection {
    let ContextSelectionInput {
        recent_turns,
        recent_turns_summary,
        knowledge_query,
        memory_query,
        shared_context,
        file_summaries,
    } = input;
    let (selected_recent_turns, turns_truncation) =
        select_limited(recent_turns, budget.max_turns, "recent_turns");
    let recent_turns_summary =
        finalize_recent_turns_summary(recent_turns_summary, &selected_recent_turns);
    let selected_turns = selected_recent_turns
        .iter()
        .map(|turn| turn.content.clone())
        .collect::<Vec<_>>();

    let (selected_knowledge, knowledge_truncation) =
        select_knowledge(runtime, budget, knowledge_query);
    let (selected_memory, memory_truncation) = select_memory(runtime, budget, memory_query);
    let (selected_shared_context, shared_truncation) =
        select_limited(shared_context, budget.max_shared_items, "shared_context");
    let (selected_file_summaries, file_truncation) =
        select_limited(file_summaries, budget.max_file_summaries, "file_summaries");

    let truncations = [
        turns_truncation,
        knowledge_truncation,
        memory_truncation,
        shared_truncation,
        file_truncation,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    BudgetedContextSelection {
        selected_recent_turns,
        selected_turns,
        selected_knowledge,
        selected_memory,
        selected_shared_context,
        selected_file_summaries,
        recent_turns_summary,
        truncations,
    }
}

fn finalize_recent_turns_summary(
    mut summary: RecentTurnsResolutionSummary,
    selected_recent_turns: &[RecentTurnRecord],
) -> RecentTurnsResolutionSummary {
    summary.resolved_count = selected_recent_turns.len().max(summary.resolved_count);
    summary.retained_count = selected_recent_turns.len();
    summary.session_source_count =
        count_recent_turns_by_source(selected_recent_turns, RecentTurnSource::Session);
    summary.project_source_count =
        count_recent_turns_by_source(selected_recent_turns, RecentTurnSource::Project);
    summary.provided_source_count =
        count_recent_turns_by_source(selected_recent_turns, RecentTurnSource::Provided);
    summary
}

fn count_recent_turns_by_source(
    selected_recent_turns: &[RecentTurnRecord],
    source: RecentTurnSource,
) -> usize {
    selected_recent_turns
        .iter()
        .filter(|turn| turn.source == source)
        .count()
}

fn select_knowledge(
    runtime: &ContextRuntime,
    budget: &ContextBudget,
    mut knowledge_query: KnowledgeQuery,
) -> (Vec<GovernedKnowledgeOutput>, Option<TruncationRecord>) {
    knowledge_query.limit = budget.max_knowledge.min(knowledge_query.limit);
    let knowledge_query_result = runtime.knowledge_store.governed_query(&knowledge_query);
    let knowledge_truncation = knowledge_query_result
        .truncated
        .then_some(TruncationRecord {
            part: "knowledge".to_string(),
            original_count: knowledge_query_result.total_matches,
            retained_count: knowledge_query_result.results.len(),
        });
    (knowledge_query_result.results, knowledge_truncation)
}

fn select_memory(
    runtime: &ContextRuntime,
    budget: &ContextBudget,
    mut memory_query: MemoryQuery,
) -> (Vec<MemoryRecord>, Option<TruncationRecord>) {
    memory_query.limit = budget.max_memory.min(memory_query.limit);
    // memory_store.query 默认按 created_at ASC 排序——直接 take(N) 会拿到最旧的记忆，
    // 与"会话记忆越近越重要"的产品语义相反。这里改为：从全量里拿最新 N 条，
    // 再按时间正序投放，最终 prompt 内的记忆按"旧→新"展开，符合 LLM 阅读直觉。
    let mut all_memory = runtime.memory_store.query(&MemoryQuery {
        limit: usize::MAX,
        ..memory_query.clone()
    });
    let all_memory_count = all_memory.len();
    if all_memory_count > memory_query.limit {
        let drop_count = all_memory_count - memory_query.limit;
        all_memory.drain(..drop_count);
    }
    let memory_truncation = (all_memory_count > memory_query.limit).then_some(TruncationRecord {
        part: "memory".to_string(),
        original_count: all_memory_count,
        retained_count: all_memory.len(),
    });
    (all_memory, memory_truncation)
}

fn select_limited<T>(
    values: Vec<T>,
    max_items: usize,
    part: &str,
) -> (Vec<T>, Option<TruncationRecord>) {
    let original_count = values.len();
    let retained = values.into_iter().take(max_items).collect::<Vec<_>>();
    let truncation = (original_count > max_items).then_some(TruncationRecord {
        part: part.to_string(),
        original_count,
        retained_count: retained.len(),
    });
    (retained, truncation)
}
