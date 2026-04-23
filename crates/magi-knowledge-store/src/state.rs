use crate::{CodeIndexSource, KnowledgeAuditLink, KnowledgeGovernanceLink, KnowledgeRecord};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KnowledgeState {
    pub(crate) entries: HashMap<String, KnowledgeRecord>,
    pub(crate) index_terms: HashMap<String, Vec<String>>,
    pub(crate) code_sources: HashMap<String, CodeIndexSource>,
    pub(crate) audit_links: HashMap<String, KnowledgeAuditLink>,
    pub(crate) governance_links: HashMap<String, KnowledgeGovernanceLink>,
}

impl KnowledgeState {
    pub(crate) fn upsert(
        &mut self,
        record: KnowledgeRecord,
        indexed_terms: Vec<String>,
        code_source: Option<CodeIndexSource>,
        audit_link: Option<KnowledgeAuditLink>,
        governance_link: Option<KnowledgeGovernanceLink>,
    ) {
        self.index_terms
            .insert(record.knowledge_id.clone(), indexed_terms);
        Self::set_sidecar(&record.knowledge_id, &mut self.code_sources, code_source);
        Self::set_sidecar(&record.knowledge_id, &mut self.audit_links, audit_link);
        Self::set_sidecar(
            &record.knowledge_id,
            &mut self.governance_links,
            governance_link,
        );
        self.entries.insert(record.knowledge_id.clone(), record);
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
