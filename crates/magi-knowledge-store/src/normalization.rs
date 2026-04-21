use crate::{
    CodeIndexIngestion, CodeIndexSource, CodeIndexSymbol, KnowledgeAuditLink,
    KnowledgeGovernanceLink, KnowledgeRecord,
};

pub(crate) fn normalize_record(mut record: KnowledgeRecord) -> KnowledgeRecord {
    record.knowledge_id = normalize_required_text(record.knowledge_id);
    record.title = normalize_required_text(record.title);
    record.content = record.content.trim().to_string();
    record.tags = normalize_tags(record.tags);
    record.source_ref = normalize_optional_text(record.source_ref);
    record
}

pub(crate) fn normalize_code_index_ingestion(mut ingestion: CodeIndexIngestion) -> CodeIndexIngestion {
    ingestion.knowledge_id = normalize_required_text(ingestion.knowledge_id);
    ingestion.title = normalize_required_text(ingestion.title);
    ingestion.content = ingestion.content.trim().to_string();
    ingestion.tags = normalize_tags(ingestion.tags);
    ingestion.source_ref = normalize_optional_text(ingestion.source_ref);
    ingestion.source = normalize_code_index_source(ingestion.source);
    ingestion.audit = normalize_audit_link(ingestion.audit);
    ingestion.governance = normalize_governance_link(ingestion.governance);
    ingestion
}

pub(crate) fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut normalized = tags
        .into_iter()
        .map(|tag| tag.trim().to_ascii_lowercase())
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

pub(crate) fn tokenize(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|part| !part.is_empty())
        .collect()
}

pub(crate) fn merge_unique_terms(terms: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut merged = terms.into_iter().collect::<Vec<_>>();
    merged.sort();
    merged.dedup();
    merged
}

pub(crate) fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_required_text(value: String) -> String {
    value.trim().to_string()
}

fn normalize_code_index_source(mut source: CodeIndexSource) -> CodeIndexSource {
    source.path = normalize_required_text(source.path);
    source.language = normalize_optional_text(source.language);
    source.repo_ref = normalize_optional_text(source.repo_ref);
    source.commit_ref = normalize_optional_text(source.commit_ref);
    source.symbol = normalize_code_index_symbol(source.symbol);
    source
}

fn normalize_code_index_symbol(symbol: Option<CodeIndexSymbol>) -> Option<CodeIndexSymbol> {
    let mut symbol = symbol?;
    symbol.name = normalize_required_text(symbol.name);
    if symbol.name.is_empty() {
        return None;
    }

    symbol.container = normalize_optional_text(symbol.container);
    symbol.signature = normalize_optional_text(symbol.signature);
    Some(symbol)
}

fn normalize_audit_link(link: Option<KnowledgeAuditLink>) -> Option<KnowledgeAuditLink> {
    let mut link = link?;
    link.audit_event_id = normalize_required_text(link.audit_event_id);
    if link.audit_event_id.is_empty() {
        return None;
    }

    link.trail_ref = normalize_optional_text(link.trail_ref);
    Some(link)
}

fn normalize_governance_link(
    link: Option<KnowledgeGovernanceLink>,
) -> Option<KnowledgeGovernanceLink> {
    let mut link = link?;
    link.policy_refs = normalize_tags(link.policy_refs);
    link.rationale = normalize_optional_text(link.rationale);
    link.audit_event_id = normalize_optional_text(link.audit_event_id);
    Some(link)
}

pub(crate) fn summarize_excerpt(content: &str) -> String {
    const MAX_CHARS: usize = 96;
    let mut excerpt = content.chars().take(MAX_CHARS).collect::<String>();
    if content.chars().count() > MAX_CHARS {
        excerpt.push('…');
    }
    excerpt
}
