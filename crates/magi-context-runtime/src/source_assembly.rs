use super::{
    ContextRuntime, FileSummaryItem, FileSummaryQuery, ProjectRecentTurnsQuery, RecentTurnRecord,
    RecentTurnSource, RecentTurnsResolutionSummary, RecentTurnsSourceQuery, SharedContextItem,
    SharedContextQuery,
};
use magi_core::UtcMillis;
use std::collections::HashSet;

pub(super) struct RuntimeAssemblySources {
    pub recent_turns: Vec<RecentTurnRecord>,
    pub recent_turns_summary: RecentTurnsResolutionSummary,
    pub shared_context: Vec<SharedContextItem>,
    pub file_summaries: Vec<FileSummaryItem>,
}

pub(super) fn provided_recent_turns(
    recent_turns: Vec<String>,
) -> (Vec<RecentTurnRecord>, RecentTurnsResolutionSummary) {
    let recent_turn_count = recent_turns.len();
    let recent_turns = recent_turns
        .into_iter()
        .map(|content| RecentTurnRecord {
            source: RecentTurnSource::Provided,
            content,
            updated_at: UtcMillis::now(),
        })
        .collect::<Vec<_>>();

    (
        recent_turns,
        RecentTurnsResolutionSummary {
            resolved_count: recent_turn_count,
            retained_count: 0,
            session_source_count: 0,
            project_source_count: 0,
            provided_source_count: 0,
        },
    )
}

pub(super) fn resolve_runtime_sources(
    runtime: &ContextRuntime,
    recent_turns_query: &RecentTurnsSourceQuery,
    shared_context_query: &SharedContextQuery,
    file_summary_query: &FileSummaryQuery,
) -> RuntimeAssemblySources {
    let recent_turns = resolve_recent_turns(runtime, recent_turns_query);
    let recent_turns_summary = RecentTurnsResolutionSummary {
        resolved_count: recent_turns.len(),
        retained_count: 0,
        session_source_count: 0,
        project_source_count: 0,
        provided_source_count: 0,
    };
    let shared_context = runtime
        .shared_context_pool
        .query(shared_context_query)
        .into_iter()
        .map(|record| record.item)
        .collect::<Vec<_>>();
    let file_summaries = runtime
        .file_summary_store
        .query(file_summary_query)
        .into_iter()
        .map(|record| record.item)
        .collect::<Vec<_>>();

    RuntimeAssemblySources {
        recent_turns,
        recent_turns_summary,
        shared_context,
        file_summaries,
    }
}

fn resolve_recent_turns(
    runtime: &ContextRuntime,
    query: &RecentTurnsSourceQuery,
) -> Vec<RecentTurnRecord> {
    let session_limit = query.max_session_turns.unwrap_or(query.limit);
    let project_limit = query.max_project_turns.unwrap_or(query.limit);
    let mut merged = Vec::new();

    if let Some(session_id) = query.session_id.as_ref() {
        let session_turns = runtime
            .session_store
            .as_ref()
            .map(|session_store| session_store.timeline_for_session(session_id))
            .unwrap_or_default()
            .into_iter()
            .rev()
            .take(session_limit)
            .map(|entry| RecentTurnRecord {
                source: RecentTurnSource::Session,
                content: entry.message,
                updated_at: entry.occurred_at,
            })
            .collect::<Vec<_>>();
        merged.extend(session_turns);
    }

    if let Some(project_key) = query.project_key.as_ref() {
        let project_turns = runtime
            .project_recent_turn_store
            .query(&ProjectRecentTurnsQuery {
                project_key: project_key.clone(),
                limit: project_limit,
            })
            .into_iter()
            .map(|record| RecentTurnRecord {
                source: RecentTurnSource::Project,
                content: record.content,
                updated_at: record.updated_at,
            })
            .collect::<Vec<_>>();
        merged.extend(project_turns);
    }

    merged.sort_by(|left, right| {
        left.updated_at
            .0
            .cmp(&right.updated_at.0)
            .then_with(|| left.source.priority().cmp(&right.source.priority()))
            .then_with(|| left.content.cmp(&right.content))
    });

    if query.deduplicate {
        merged = deduplicate_recent_turns(merged);
    }

    if query.limit < merged.len() {
        let start = merged.len() - query.limit;
        merged = merged.split_off(start);
    }

    merged
}

fn deduplicate_recent_turns(records: Vec<RecentTurnRecord>) -> Vec<RecentTurnRecord> {
    let mut seen = HashSet::new();
    let mut deduplicated = records
        .into_iter()
        .rev()
        .filter(|record| seen.insert(record.content.clone()))
        .collect::<Vec<_>>();
    deduplicated.reverse();
    deduplicated
}
