use crate::{
    CodeIndexSource, GovernedKnowledgeOutput, GovernedKnowledgeService, KnowledgeAuditLink,
    KnowledgeGovernanceLink, KnowledgeQueryResult, normalization::summarize_excerpt,
};
use std::collections::HashMap;

impl GovernedKnowledgeService {
    pub fn project(
        result: KnowledgeQueryResult,
        code_sources: &HashMap<String, CodeIndexSource>,
        audit_links: &HashMap<String, KnowledgeAuditLink>,
        governance_links: &HashMap<String, KnowledgeGovernanceLink>,
    ) -> Vec<GovernedKnowledgeOutput> {
        result
            .matches
            .into_iter()
            .map(|knowledge_match| {
                let record = knowledge_match.record;
                let knowledge_id = record.knowledge_id.clone();

                GovernedKnowledgeOutput {
                    knowledge_id: knowledge_id.clone(),
                    title: record.title,
                    kind: record.kind,
                    excerpt: summarize_excerpt(&record.content),
                    updated_at: record.updated_at,
                    score: knowledge_match.score,
                    matched_terms: knowledge_match.matched_terms,
                    source_ref: record.source_ref,
                    code_source: code_sources.get(&knowledge_id).cloned(),
                    audit_link: audit_links.get(&knowledge_id).cloned(),
                    governance_link: governance_links.get(&knowledge_id).cloned(),
                }
            })
            .collect()
    }
}
