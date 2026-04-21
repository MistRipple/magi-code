use magi_core::UtcMillis;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeSymbolKind {
    Module,
    Struct,
    Enum,
    Trait,
    Function,
    Method,
    Constant,
    Static,
    TypeAlias,
}

impl CodeSymbolKind {
    pub(crate) fn as_index_term(&self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Function => "function",
            Self::Method => "method",
            Self::Constant => "constant",
            Self::Static => "static",
            Self::TypeAlias => "typealias",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeIndexSymbol {
    pub name: String,
    pub kind: CodeSymbolKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeIndexSource {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<CodeIndexSymbol>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeAuditLink {
    pub audit_event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnowledgeGovernanceOutcome {
    Allowed,
    NeedsApproval,
    Blocked,
    Rejected,
}

impl KnowledgeGovernanceOutcome {
    pub(crate) fn as_index_term(&self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::NeedsApproval => "needsapproval",
            Self::Blocked => "blocked",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeGovernanceLink {
    pub outcome: KnowledgeGovernanceOutcome,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_event_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeIndexIngestion {
    pub knowledge_id: String,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub source_ref: Option<String>,
    pub updated_at: UtcMillis,
    pub source: CodeIndexSource,
    pub audit: Option<KnowledgeAuditLink>,
    pub governance: Option<KnowledgeGovernanceLink>,
}
