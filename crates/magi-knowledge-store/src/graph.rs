use magi_core::{UtcMillis, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::dependency_graph::DependencyEdge;
use crate::symbol_index::{SymbolEntry, SymbolKind};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeKind {
    #[default]
    Workspace,
    File,
    Symbol,
    Knowledge,
}

impl GraphNodeKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "workspace" => Some(Self::Workspace),
            "file" => Some(Self::File),
            "symbol" => Some(Self::Symbol),
            "knowledge" => Some(Self::Knowledge),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphEdgeKind {
    Contains,
    DependsOn,
    AppliesTo,
    Explains,
    References,
    RelatedTo,
    Supersedes,
    Contradicts,
}

impl GraphEdgeKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "contains" => Some(Self::Contains),
            "depends_on" | "dependson" => Some(Self::DependsOn),
            "applies_to" | "appliesto" => Some(Self::AppliesTo),
            "explains" => Some(Self::Explains),
            "references" | "refs" => Some(Self::References),
            "related_to" | "relatedto" => Some(Self::RelatedTo),
            "supersedes" => Some(Self::Supersedes),
            "contradicts" => Some(Self::Contradicts),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphDirection {
    Forward,
    Reverse,
    #[default]
    Both,
}

impl GraphDirection {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "forward" | "out" => Some(Self::Forward),
            "reverse" | "backward" | "in" => Some(Self::Reverse),
            "both" | "all" => Some(Self::Both),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphEdgeOrigin {
    DeterministicCode,
    ExplicitUser,
    ExplicitAgent,
    Inferred,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphEdgeStatus {
    #[default]
    Active,
    Candidate,
    Dangling,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum GraphNodeRef {
    Knowledge {
        #[serde(rename = "knowledgeId")]
        knowledge_id: String,
    },
    File {
        path: String,
    },
    Symbol {
        path: String,
        #[serde(rename = "qualifiedName")]
        qualified_name: String,
        #[serde(rename = "symbolKind")]
        symbol_kind: String,
    },
}

impl GraphNodeRef {
    pub fn id(&self) -> String {
        match self {
            Self::Knowledge { knowledge_id } => format!("knowledge:{knowledge_id}"),
            Self::File { path } => format!("file:{path}"),
            Self::Symbol {
                path,
                qualified_name,
                symbol_kind,
            } => format!("symbol:{path}:{qualified_name}:{symbol_kind}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeRelation {
    pub relation_id: String,
    pub workspace_id: WorkspaceId,
    pub source: GraphNodeRef,
    pub kind: GraphEdgeKind,
    pub target: GraphNodeRef,
    pub origin: GraphEdgeOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    pub status: GraphEdgeStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// 自动发现候选的稳定指纹。手动关系不设置此字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_key: Option<String>,
    /// 修正或审阅后仍保留的原始自动发现证据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_evidence: Option<Vec<String>>,
    /// 用户完成确认或忽略后的审阅时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<UtcMillis>,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    pub id: String,
    pub kind: GraphNodeKind,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub knowledge_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_kind: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub kind: GraphEdgeKind,
    pub label: String,
    pub origin: GraphEdgeOrigin,
    pub status: GraphEdgeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphStats {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub returned_nodes: usize,
    pub returned_edges: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeGraph {
    pub workspace_id: WorkspaceId,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub stats: GraphStats,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    pub depth: usize,
    pub direction: GraphDirection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_kinds: Vec<GraphNodeKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edge_kinds: Vec<GraphEdgeKind>,
    pub max_nodes: usize,
    pub max_edges: usize,
}

impl Default for GraphQuery {
    fn default() -> Self {
        Self {
            focus: None,
            depth: 1,
            direction: GraphDirection::Both,
            node_kinds: Vec::new(),
            edge_kinds: Vec::new(),
            max_nodes: 120,
            max_edges: 240,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CodeGraphSnapshot {
    pub files: Vec<String>,
    pub dependency_edges: Vec<DependencyEdge>,
    pub symbols: Vec<SymbolEntry>,
}

pub(crate) fn symbol_kind_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Class => "class",
        SymbolKind::Interface => "interface",
        SymbolKind::Type => "type",
        SymbolKind::Enum => "enum",
        SymbolKind::Variable => "variable",
        SymbolKind::Method => "method",
    }
}
