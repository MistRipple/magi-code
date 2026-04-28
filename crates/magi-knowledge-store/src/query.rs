use crate::{
    CodeIndexSource, KnowledgeAuditLink, KnowledgeGovernanceLink, KnowledgeIndexer, KnowledgeMatch,
    KnowledgeQuery, KnowledgeQueryResult, KnowledgeQueryService, KnowledgeRecord,
    normalization::{merge_unique_terms, normalize_tags, tokenize},
};
use std::collections::{HashMap, HashSet};

impl KnowledgeQueryService {
    pub fn execute(
        entries: &HashMap<String, KnowledgeRecord>,
        index_terms: &HashMap<String, Vec<String>>,
        code_sources: &HashMap<String, CodeIndexSource>,
        audit_links: &HashMap<String, KnowledgeAuditLink>,
        governance_links: &HashMap<String, KnowledgeGovernanceLink>,
        query: &KnowledgeQuery,
    ) -> KnowledgeQueryResult {
        let query_terms = query.text.as_deref().map(tokenize).unwrap_or_default();
        let normalized_tags = normalize_tags(query.tags.clone());

        let mut matches = entries
            .values()
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
                let term_set = indexed_terms.iter().cloned().collect::<HashSet<_>>();

                if !normalized_tags
                    .iter()
                    .all(|tag| record.tags.iter().any(|record_tag| record_tag == tag))
                {
                    return None;
                }

                let matched_query_terms = query_terms
                    .iter()
                    .filter(|term| term_set.contains(term.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();

                if !query_terms.is_empty() && matched_query_terms.is_empty() {
                    return None;
                }

                let matched_tags = normalized_tags
                    .iter()
                    .filter(|tag| record.tags.iter().any(|record_tag| record_tag == *tag))
                    .cloned()
                    .collect::<Vec<_>>();

                let score = matched_query_terms.len() * 3 + matched_tags.len() * 2;
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
