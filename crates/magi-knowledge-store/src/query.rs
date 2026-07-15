use crate::{
    CodeIndexSource, KnowledgeAuditLink, KnowledgeGovernanceLink, KnowledgeIndexer, KnowledgeKind,
    KnowledgeMatch, KnowledgeQuery, KnowledgeQueryResult, KnowledgeQueryService, KnowledgeRecord,
    normalization::{merge_unique_terms, normalize_tags, tokenize_business_text},
};
use std::collections::{HashMap, HashSet};

impl KnowledgeQueryService {
    pub fn execute(
        entries: &HashMap<String, KnowledgeRecord>,
        index_terms: &HashMap<String, Vec<String>>,
        term_postings: &HashMap<String, HashSet<String>>,
        code_sources: &HashMap<String, CodeIndexSource>,
        audit_links: &HashMap<String, KnowledgeAuditLink>,
        governance_links: &HashMap<String, KnowledgeGovernanceLink>,
        query: &KnowledgeQuery,
    ) -> KnowledgeQueryResult {
        let query_terms = query
            .text
            .as_deref()
            .map(tokenize_business_text)
            .unwrap_or_default();
        let normalized_tags = normalize_tags(query.tags.clone());

        let candidate_ids = if query_terms.is_empty() {
            None
        } else {
            Some(
                query_terms
                    .iter()
                    .filter_map(|term| term_postings.get(term))
                    .flat_map(|posting| posting.iter().cloned())
                    .collect::<HashSet<_>>(),
            )
        };

        let mut matches = entries
            .values()
            .filter(|record| {
                candidate_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&record.knowledge_id))
            })
            .filter(|record| {
                query
                    .workspace_id
                    .as_ref()
                    .is_none_or(|workspace_id| record.workspace_id.as_ref() == Some(workspace_id))
            })
            .filter(|record| query.kind.is_none_or(|kind| record.kind == kind))
            .filter_map(|record| {
                let indexed_terms = index_terms
                    .get(&record.knowledge_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        KnowledgeIndexer::build_terms_with_context(
                            record,
                            code_sources.get(&record.knowledge_id),
                            audit_links.get(&record.knowledge_id),
                            governance_links.get(&record.knowledge_id),
                        )
                    });
                if !normalized_tags
                    .iter()
                    .all(|tag| record.tags.iter().any(|record_tag| record_tag == tag))
                {
                    return None;
                }

                let (matched_query_terms, query_score) = if record.kind == KnowledgeKind::CodeIndex
                {
                    let matched = query_terms
                        .iter()
                        .filter(|term| indexed_terms.contains(term))
                        .cloned()
                        .collect::<Vec<_>>();
                    let score = matched.len() * 3;
                    (matched, score)
                } else {
                    score_business_fields(record, &query_terms)
                };

                if !query_terms.is_empty() && matched_query_terms.is_empty() {
                    return None;
                }

                let matched_tags = normalized_tags
                    .iter()
                    .filter(|tag| record.tags.iter().any(|record_tag| record_tag == *tag))
                    .cloned()
                    .collect::<Vec<_>>();

                let score = query_score + matched_tags.len() * 2;
                let matched_terms =
                    merge_unique_terms(matched_query_terms.into_iter().chain(matched_tags));

                Some(KnowledgeMatch {
                    record: record.clone(),
                    score,
                    matched_terms,
                })
            })
            .collect::<Vec<_>>();

        matches.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| right.record.updated_at.0.cmp(&left.record.updated_at.0))
                .then_with(|| left.record.knowledge_id.cmp(&right.record.knowledge_id))
        });

        let total_matches = matches.len();
        let limit = query.limit;
        let truncated = total_matches > limit;
        if limit < matches.len() {
            matches.truncate(limit);
        }

        let records = matches
            .iter()
            .map(|knowledge_match| knowledge_match.record.clone())
            .collect::<Vec<_>>();

        KnowledgeQueryResult {
            records,
            matches,
            total_matches,
            truncated,
        }
    }
}

fn score_business_fields(record: &KnowledgeRecord, query_terms: &[String]) -> (Vec<String>, usize) {
    let title_terms = tokenize_business_text(&record.title);
    let content_terms = tokenize_business_text(&record.content);
    let tag_terms = record
        .tags
        .iter()
        .flat_map(|tag| tokenize_business_text(tag))
        .collect::<HashSet<_>>();

    let mut matched_terms = Vec::new();
    let mut field_score = 0usize;
    for term in query_terms {
        let weight = if title_terms.contains(term) {
            9
        } else if tag_terms.contains(term) {
            7
        } else if content_terms.contains(term) {
            4
        } else {
            0
        };
        if weight > 0 {
            matched_terms.push(term.clone());
            field_score += weight;
        }
    }

    let coverage_score = if query_terms.is_empty() {
        0
    } else {
        matched_terms.len() * 100 / query_terms.len()
    };
    (matched_terms, coverage_score + field_score)
}
