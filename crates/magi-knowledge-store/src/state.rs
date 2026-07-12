use crate::{CodeIndexSource, KnowledgeAuditLink, KnowledgeGovernanceLink, KnowledgeRecord};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KnowledgeState {
    pub(crate) entries: HashMap<String, KnowledgeRecord>,
    pub(crate) index_terms: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub(crate) term_postings: HashMap<String, HashSet<String>>,
    pub(crate) code_sources: HashMap<String, CodeIndexSource>,
    pub(crate) audit_links: HashMap<String, KnowledgeAuditLink>,
    pub(crate) governance_links: HashMap<String, KnowledgeGovernanceLink>,
}

impl KnowledgeState {
    pub(crate) fn upsert(
        &mut self,
        mut record: KnowledgeRecord,
        indexed_terms: Vec<String>,
        code_source: Option<CodeIndexSource>,
        audit_link: Option<KnowledgeAuditLink>,
        governance_link: Option<KnowledgeGovernanceLink>,
    ) {
        self.remove_term_postings(&record.knowledge_id);
        if let Some(existing) = self.entries.get(&record.knowledge_id) {
            record.created_at = existing.created_at;
        } else if record.created_at.0 == 0 {
            record.created_at = record.updated_at;
        }
        self.index_terms
            .insert(record.knowledge_id.clone(), indexed_terms.clone());
        for term in indexed_terms {
            self.term_postings
                .entry(term)
                .or_default()
                .insert(record.knowledge_id.clone());
        }
        Self::set_sidecar(&record.knowledge_id, &mut self.code_sources, code_source);
        Self::set_sidecar(&record.knowledge_id, &mut self.audit_links, audit_link);
        Self::set_sidecar(
            &record.knowledge_id,
            &mut self.governance_links,
            governance_link,
        );
        self.entries.insert(record.knowledge_id.clone(), record);
    }

    pub(crate) fn delete(&mut self, knowledge_id: &str) -> bool {
        if self.entries.remove(knowledge_id).is_none() {
            return false;
        }
        self.remove_term_postings(knowledge_id);
        self.index_terms.remove(knowledge_id);
        self.code_sources.remove(knowledge_id);
        self.audit_links.remove(knowledge_id);
        self.governance_links.remove(knowledge_id);
        true
    }

    pub(crate) fn rebuild_term_postings(&mut self) {
        self.term_postings.clear();
        for (knowledge_id, terms) in &self.index_terms {
            for term in terms {
                self.term_postings
                    .entry(term.clone())
                    .or_default()
                    .insert(knowledge_id.clone());
            }
        }
    }

    fn remove_term_postings(&mut self, knowledge_id: &str) {
        let Some(terms) = self.index_terms.get(knowledge_id) else {
            return;
        };
        for term in terms {
            if let Some(posting) = self.term_postings.get_mut(term) {
                posting.remove(knowledge_id);
                if posting.is_empty() {
                    self.term_postings.remove(term);
                }
            }
        }
    }

    pub(crate) fn get(&self, knowledge_id: &str) -> Option<KnowledgeRecord> {
        self.entries.get(knowledge_id).cloned()
    }

    pub(crate) fn list(&self) -> Vec<KnowledgeRecord> {
        let mut records = self.entries.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            right
                .updated_at
                .0
                .cmp(&left.updated_at.0)
                .then_with(|| left.knowledge_id.cmp(&right.knowledge_id))
        });
        records
    }

    pub(crate) fn indexed_terms(&self, knowledge_id: &str) -> Vec<String> {
        self.index_terms
            .get(knowledge_id)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn code_source(&self, knowledge_id: &str) -> Option<CodeIndexSource> {
        self.code_sources.get(knowledge_id).cloned()
    }

    pub(crate) fn audit_link(&self, knowledge_id: &str) -> Option<KnowledgeAuditLink> {
        self.audit_links.get(knowledge_id).cloned()
    }

    pub(crate) fn governance_link(&self, knowledge_id: &str) -> Option<KnowledgeGovernanceLink> {
        self.governance_links.get(knowledge_id).cloned()
    }

    fn set_sidecar<T>(knowledge_id: &str, target: &mut HashMap<String, T>, value: Option<T>) {
        match value {
            Some(value) => {
                target.insert(knowledge_id.to_string(), value);
            }
            None => {
                target.remove(knowledge_id);
            }
        }
    }
}
