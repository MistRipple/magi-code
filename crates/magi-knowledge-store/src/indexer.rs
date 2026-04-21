use crate::{
    normalization::tokenize, CodeIndexSource, KnowledgeAuditLink, KnowledgeGovernanceLink,
    KnowledgeIndexer, KnowledgeRecord,
};

impl KnowledgeIndexer {
    pub fn build_terms(record: &KnowledgeRecord) -> Vec<String> {
        Self::build_terms_with_context(record, None, None, None)
    }

    pub(crate) fn build_terms_with_context(
        record: &KnowledgeRecord,
        code_source: Option<&CodeIndexSource>,
        audit_link: Option<&KnowledgeAuditLink>,
        governance_link: Option<&KnowledgeGovernanceLink>,
    ) -> Vec<String> {
        let mut terms = tokenize(&record.title);
        terms.extend(tokenize(&record.content));
        terms.extend(record.tags.iter().map(|tag| tag.to_ascii_lowercase()));
        if let Some(source_ref) = &record.source_ref {
            terms.extend(tokenize(source_ref));
        }

        if let Some(code_source) = code_source {
            terms.extend(tokenize(&code_source.path));

            if let Some(language) = &code_source.language {
                terms.extend(tokenize(language));
            }
            if let Some(repo_ref) = &code_source.repo_ref {
                terms.extend(tokenize(repo_ref));
            }
            if let Some(commit_ref) = &code_source.commit_ref {
                terms.extend(tokenize(commit_ref));
            }
            if let Some(start_line) = code_source.start_line {
                terms.push(start_line.to_string());
            }
            if let Some(end_line) = code_source.end_line {
                terms.push(end_line.to_string());
            }
            if let Some(symbol) = &code_source.symbol {
                terms.extend(tokenize(&symbol.name));
                terms.push(symbol.kind.as_index_term().to_string());
                if let Some(container) = &symbol.container {
                    terms.extend(tokenize(container));
                }
                if let Some(signature) = &symbol.signature {
                    terms.extend(tokenize(signature));
                }
            }
        }

        if let Some(audit_link) = audit_link {
            terms.extend(tokenize(&audit_link.audit_event_id));
            if let Some(trail_ref) = &audit_link.trail_ref {
                terms.extend(tokenize(trail_ref));
            }
            if let Some(sequence) = audit_link.sequence {
                terms.push(sequence.to_string());
            }
        }

        if let Some(governance_link) = governance_link {
            terms.push(governance_link.outcome.as_index_term().to_string());
            terms.extend(
                governance_link
                    .policy_refs
                    .iter()
                    .flat_map(|policy_ref| tokenize(policy_ref)),
            );
            if let Some(rationale) = &governance_link.rationale {
                terms.extend(tokenize(rationale));
            }
            if let Some(audit_event_id) = &governance_link.audit_event_id {
                terms.extend(tokenize(audit_event_id));
            }
        }

        terms.sort();
        terms.dedup();
        terms
    }
}
