use crate::graph::{
    CodeGraphSnapshot, GraphDirection, GraphEdge, GraphEdgeKind, GraphEdgeOrigin, GraphEdgeStatus,
    GraphNode, GraphNodeKind, GraphQuery, GraphStats, KnowledgeGraph, symbol_kind_label,
};
use crate::{KnowledgeKind, KnowledgeRecord, KnowledgeRelation};
use magi_core::WorkspaceId;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

const MAX_GRAPH_DEPTH: usize = 3;
const MAX_GRAPH_NODES: usize = 120;
const MAX_GRAPH_EDGES: usize = 240;

pub(crate) fn build_workspace_graph(
    workspace_id: &WorkspaceId,
    code: CodeGraphSnapshot,
    knowledge: Vec<KnowledgeRecord>,
    relations: Vec<KnowledgeRelation>,
    query: &GraphQuery,
) -> KnowledgeGraph {
    let query = normalize_query(query);
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let workspace_node_id = format!("workspace:{}", workspace_id.as_str());

    nodes.push(GraphNode {
        id: workspace_node_id.clone(),
        kind: GraphNodeKind::Workspace,
        label: workspace_id.as_str().to_string(),
        path: None,
        knowledge_id: None,
        symbol_kind: None,
        metadata: BTreeMap::new(),
    });

    let mut known_file_ids = HashSet::new();
    for path in code.files {
        let id = format!("file:{path}");
        known_file_ids.insert(id.clone());
        nodes.push(GraphNode {
            id: id.clone(),
            kind: GraphNodeKind::File,
            label: path.clone(),
            path: Some(path.clone()),
            knowledge_id: None,
            symbol_kind: None,
            metadata: BTreeMap::new(),
        });
        edges.push(derived_edge(
            format!("{workspace_node_id}-contains-{id}"),
            workspace_node_id.clone(),
            id,
            GraphEdgeKind::Contains,
            vec![format!("workspace:{workspace_id};path:{path}")],
        ));
    }

    for symbol in code.symbols {
        let kind = symbol_kind_label(symbol.kind).to_string();
        let qualified_name = symbol_qualified_name(&symbol);
        let id = format!("symbol:{}:{}:{}", symbol.file_path, qualified_name, kind);
        let file_id = format!("file:{}", symbol.file_path);
        if !known_file_ids.contains(&file_id) {
            continue;
        }
        let mut metadata = BTreeMap::new();
        metadata.insert("line".to_string(), symbol.line.to_string());
        if let Some(end_line) = symbol.end_line {
            metadata.insert("endLine".to_string(), end_line.to_string());
        }
        if let Some(container) = symbol.container {
            metadata.insert("container".to_string(), container);
        }
        nodes.push(GraphNode {
            id: id.clone(),
            kind: GraphNodeKind::Symbol,
            label: qualified_name,
            path: Some(symbol.file_path.clone()),
            knowledge_id: None,
            symbol_kind: Some(kind),
            metadata,
        });
        edges.push(derived_edge(
            format!("{file_id}-contains-{id}"),
            file_id,
            id,
            GraphEdgeKind::Contains,
            vec![format!("path:{}", symbol.file_path)],
        ));
    }

    let mut dependency_edges = BTreeMap::new();
    for dependency in code.dependency_edges {
        let source = format!("file:{}", dependency.from);
        let target = format!("file:{}", dependency.to);
        if !known_file_ids.contains(&source) || !known_file_ids.contains(&target) {
            continue;
        }
        let edge = derived_edge(
            format!("{source}-depends_on-{target}"),
            source.clone(),
            target.clone(),
            GraphEdgeKind::DependsOn,
            vec![format!("import_type:{:?}", dependency.import_type)],
        );
        let entry = dependency_edges.entry(edge.id.clone()).or_insert(edge);
        let evidence = format!("import_type:{:?}", dependency.import_type);
        if !entry.evidence.contains(&evidence) {
            entry.evidence.push(evidence);
        }
    }
    edges.extend(dependency_edges.into_values());

    for record in knowledge {
        if record.kind == KnowledgeKind::CodeIndex {
            continue;
        }
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "kind".to_string(),
            knowledge_kind_label(record.kind).to_string(),
        );
        if !record.tags.is_empty() {
            metadata.insert("tags".to_string(), record.tags.join(","));
        }
        nodes.push(GraphNode {
            id: format!("knowledge:{}", record.knowledge_id),
            kind: GraphNodeKind::Knowledge,
            label: record.title,
            path: None,
            knowledge_id: Some(record.knowledge_id),
            symbol_kind: None,
            metadata,
        });
    }

    let mut known_node_ids = nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    for relation in relations {
        if relation.workspace_id != *workspace_id {
            continue;
        }
        let source = relation.source.id();
        let target = relation.target.id();
        let source_exists = known_node_ids.contains(&source);
        let target_exists = known_node_ids.contains(&target);
        if !source_exists {
            nodes.push(dangling_node(&relation.source));
            known_node_ids.insert(source.clone());
        }
        if !target_exists {
            nodes.push(dangling_node(&relation.target));
            known_node_ids.insert(target.clone());
        }
        let status = if source_exists && target_exists {
            relation.status
        } else {
            GraphEdgeStatus::Dangling
        };
        edges.push(GraphEdge {
            id: relation.relation_id,
            source,
            target,
            kind: relation.kind,
            label: edge_kind_label(relation.kind).to_string(),
            origin: relation.origin,
            status,
            confidence: relation.confidence,
            evidence: relation.evidence,
        });
    }

    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    edges.sort_by(|left, right| left.id.cmp(&right.id));
    let (nodes, edges, stats, truncated) = select_graph(nodes, edges, &query);
    KnowledgeGraph {
        workspace_id: workspace_id.clone(),
        nodes,
        edges,
        stats,
        truncated,
    }
}

fn dangling_node(reference: &crate::graph::GraphNodeRef) -> GraphNode {
    let (kind, label, path, knowledge_id, symbol_kind) = match reference {
        crate::graph::GraphNodeRef::Knowledge { knowledge_id } => (
            GraphNodeKind::Knowledge,
            knowledge_id.clone(),
            None,
            Some(knowledge_id.clone()),
            None,
        ),
        crate::graph::GraphNodeRef::File { path } => (
            GraphNodeKind::File,
            path.clone(),
            Some(path.clone()),
            None,
            None,
        ),
        crate::graph::GraphNodeRef::Symbol {
            path,
            qualified_name,
            symbol_kind,
        } => (
            GraphNodeKind::Symbol,
            qualified_name.clone(),
            Some(path.clone()),
            None,
            Some(symbol_kind.clone()),
        ),
    };
    GraphNode {
        id: reference.id(),
        kind,
        label,
        path,
        knowledge_id,
        symbol_kind,
        metadata: BTreeMap::from([("status".to_string(), "dangling".to_string())]),
    }
}

fn normalize_query(query: &GraphQuery) -> GraphQuery {
    GraphQuery {
        focus: query.focus.clone(),
        depth: query.depth.min(MAX_GRAPH_DEPTH),
        direction: query.direction,
        node_kinds: query.node_kinds.clone(),
        edge_kinds: query.edge_kinds.clone(),
        max_nodes: query.max_nodes.clamp(1, MAX_GRAPH_NODES),
        max_edges: query.max_edges.clamp(1, MAX_GRAPH_EDGES),
    }
}

fn select_graph(
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    query: &GraphQuery,
) -> (Vec<GraphNode>, Vec<GraphEdge>, GraphStats, bool) {
    let node_map: HashMap<String, GraphNode> = nodes
        .into_iter()
        .map(|node| (node.id.clone(), node))
        .collect();
    let filtered_edges: Vec<GraphEdge> = edges
        .into_iter()
        .filter(|edge| query.edge_kinds.is_empty() || query.edge_kinds.contains(&edge.kind))
        .filter(|edge| node_map.contains_key(&edge.source) && node_map.contains_key(&edge.target))
        .collect();

    let mut candidate_ids = if let Some(focus) = query.focus.as_deref() {
        collect_reachable_ids(focus, query, &node_map, &filtered_edges)
    } else {
        node_map.keys().cloned().collect::<Vec<_>>()
    };
    candidate_ids.retain(|id| {
        node_map.get(id).is_some_and(|node| {
            query.node_kinds.is_empty() || query.node_kinds.contains(&node.kind)
        })
    });
    candidate_ids.sort();

    let total_nodes = candidate_ids.len();
    let mut truncated = total_nodes > query.max_nodes;
    if let Some(focus) = query.focus.as_ref()
        && candidate_ids.iter().any(|id| id == focus)
    {
        // 截断前始终保留焦点节点。否则 workspace 聚焦图按 ID 排序时会先选中
        // 一批 file 节点，把 workspace 自身和所有 contains 边一起截掉，画布看起来
        // 像“只有节点没有关系”，无法完成局部探索。
        candidate_ids.retain(|id| id != focus);
        candidate_ids.truncate(query.max_nodes.saturating_sub(1));
        candidate_ids.push(focus.clone());
        candidate_ids.sort();
    } else {
        candidate_ids.truncate(query.max_nodes);
    }
    let selected_ids: HashSet<String> = candidate_ids.iter().cloned().collect();

    let mut selected_edges = filtered_edges
        .into_iter()
        .filter(|edge| selected_ids.contains(&edge.source) && selected_ids.contains(&edge.target))
        .collect::<Vec<_>>();
    selected_edges.sort_by(|left, right| left.id.cmp(&right.id));
    let total_edges = selected_edges.len();
    if total_edges > query.max_edges {
        truncated = true;
        selected_edges.truncate(query.max_edges);
    }

    let selected_nodes = candidate_ids
        .into_iter()
        .filter_map(|id| node_map.get(&id).cloned())
        .collect::<Vec<_>>();
    let stats = GraphStats {
        total_nodes,
        total_edges,
        returned_nodes: selected_nodes.len(),
        returned_edges: selected_edges.len(),
    };
    (selected_nodes, selected_edges, stats, truncated)
}

fn collect_reachable_ids(
    focus: &str,
    query: &GraphQuery,
    node_map: &HashMap<String, GraphNode>,
    edges: &[GraphEdge],
) -> Vec<String> {
    if !node_map.contains_key(focus) {
        return Vec::new();
    }
    let mut queue = VecDeque::from([(focus.to_string(), 0usize)]);
    let mut visited = HashSet::from([focus.to_string()]);
    while let Some((current, depth)) = queue.pop_front() {
        if depth >= query.depth {
            continue;
        }
        let mut neighbors = Vec::new();
        for edge in edges {
            if matches!(
                query.direction,
                GraphDirection::Forward | GraphDirection::Both
            ) && edge.source == current
            {
                neighbors.push(edge.target.clone());
            }
            if matches!(
                query.direction,
                GraphDirection::Reverse | GraphDirection::Both
            ) && edge.target == current
            {
                neighbors.push(edge.source.clone());
            }
        }
        neighbors.sort();
        neighbors.dedup();
        for neighbor in neighbors {
            if visited.insert(neighbor.clone()) {
                queue.push_back((neighbor, depth + 1));
            }
        }
    }
    visited.into_iter().collect()
}

fn derived_edge(
    id: String,
    source: String,
    target: String,
    kind: GraphEdgeKind,
    evidence: Vec<String>,
) -> GraphEdge {
    GraphEdge {
        id,
        source,
        target,
        kind,
        label: edge_kind_label(kind).to_string(),
        origin: GraphEdgeOrigin::DeterministicCode,
        status: GraphEdgeStatus::Active,
        confidence: None,
        evidence,
    }
}

fn edge_kind_label(kind: GraphEdgeKind) -> &'static str {
    match kind {
        GraphEdgeKind::Contains => "contains",
        GraphEdgeKind::DependsOn => "depends_on",
        GraphEdgeKind::AppliesTo => "applies_to",
        GraphEdgeKind::Explains => "explains",
        GraphEdgeKind::References => "references",
        GraphEdgeKind::RelatedTo => "related_to",
        GraphEdgeKind::Supersedes => "supersedes",
        GraphEdgeKind::Contradicts => "contradicts",
    }
}

fn symbol_qualified_name(symbol: &crate::symbol_index::SymbolEntry) -> String {
    symbol
        .container
        .as_deref()
        .filter(|container| !container.trim().is_empty())
        .map(|container| format!("{container}::{}", symbol.name))
        .unwrap_or_else(|| symbol.name.clone())
}

fn knowledge_kind_label(kind: KnowledgeKind) -> &'static str {
    match kind {
        KnowledgeKind::Adr => "adr",
        KnowledgeKind::Faq => "faq",
        KnowledgeKind::Learning => "learning",
        KnowledgeKind::CodeIndex => "code_index",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dependency_graph::{DependencyEdge, ImportType};
    use crate::symbol_index::{SymbolEntry, SymbolKind};
    use magi_core::{UtcMillis, WorkspaceId};

    #[test]
    fn graph_query_limits_and_focus_are_stable() {
        let workspace_id = WorkspaceId::new("graph-test");
        let graph = build_workspace_graph(
            &workspace_id,
            CodeGraphSnapshot {
                files: vec!["a.rs".into(), "b.rs".into(), "c.rs".into()],
                dependency_edges: vec![
                    DependencyEdge {
                        from: "a.rs".into(),
                        to: "b.rs".into(),
                        import_type: ImportType::Static,
                    },
                    DependencyEdge {
                        from: "b.rs".into(),
                        to: "c.rs".into(),
                        import_type: ImportType::Static,
                    },
                ],
                symbols: vec![SymbolEntry {
                    name: "main".into(),
                    kind: SymbolKind::Function,
                    file_path: "a.rs".into(),
                    line: 1,
                    end_line: Some(2),
                    is_exported: true,
                    container: None,
                    signature: None,
                }],
            },
            vec![KnowledgeRecord {
                knowledge_id: "adr-1".into(),
                kind: KnowledgeKind::Adr,
                title: "Decision".into(),
                content: "Content".into(),
                tags: vec![],
                workspace_id: Some(workspace_id.clone()),
                source_ref: None,
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            }],
            Vec::new(),
            &GraphQuery {
                focus: Some("file:a.rs".into()),
                depth: 1,
                direction: GraphDirection::Forward,
                max_nodes: 20,
                max_edges: 20,
                ..GraphQuery::default()
            },
        );
        let ids = graph
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["file:a.rs", "file:b.rs", "symbol:a.rs:main:function"]
        );
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.kind == GraphEdgeKind::DependsOn)
        );
        assert!(!graph.truncated);
    }

    #[test]
    fn graph_query_enforces_hard_limits() {
        let workspace_id = WorkspaceId::new("graph-limit");
        let graph = build_workspace_graph(
            &workspace_id,
            CodeGraphSnapshot {
                files: vec!["a.rs".into(), "b.rs".into(), "c.rs".into()],
                ..CodeGraphSnapshot::default()
            },
            Vec::new(),
            Vec::new(),
            &GraphQuery {
                max_nodes: 1,
                max_edges: 1,
                ..GraphQuery::default()
            },
        );
        assert_eq!(graph.nodes.len(), 1);
        assert!(graph.truncated);
        assert_eq!(graph.stats.total_nodes, 4);
    }

    #[test]
    fn graph_query_keeps_focus_node_when_truncated() {
        let workspace_id = WorkspaceId::new("graph-focus-limit");
        let graph = build_workspace_graph(
            &workspace_id,
            CodeGraphSnapshot {
                files: vec!["a.rs".into(), "b.rs".into(), "c.rs".into()],
                ..CodeGraphSnapshot::default()
            },
            Vec::new(),
            Vec::new(),
            &GraphQuery {
                focus: Some("workspace:graph-focus-limit".into()),
                max_nodes: 2,
                max_edges: 2,
                ..GraphQuery::default()
            },
        );

        assert!(
            graph
                .nodes
                .iter()
                .any(|node| node.id == "workspace:graph-focus-limit")
        );
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert!(graph.edges[0].source == "workspace:graph-focus-limit");
        assert!(graph.truncated);
    }

    #[test]
    fn graph_query_deduplicates_dependency_edges_and_respects_direction() {
        let workspace_id = WorkspaceId::new("graph-direction");
        let graph = build_workspace_graph(
            &workspace_id,
            CodeGraphSnapshot {
                files: vec!["a.rs".into(), "b.rs".into()],
                dependency_edges: vec![
                    DependencyEdge {
                        from: "a.rs".into(),
                        to: "b.rs".into(),
                        import_type: ImportType::Static,
                    },
                    DependencyEdge {
                        from: "a.rs".into(),
                        to: "b.rs".into(),
                        import_type: ImportType::Dynamic,
                    },
                ],
                ..CodeGraphSnapshot::default()
            },
            Vec::new(),
            Vec::new(),
            &GraphQuery {
                focus: Some("file:b.rs".into()),
                depth: 1,
                direction: GraphDirection::Reverse,
                ..GraphQuery::default()
            },
        );

        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|edge| edge.kind == GraphEdgeKind::DependsOn)
                .count(),
            1
        );
        assert_eq!(
            graph
                .nodes
                .iter()
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>(),
            vec!["file:a.rs", "file:b.rs", "workspace:graph-direction"]
        );
        assert_eq!(
            graph
                .edges
                .iter()
                .find(|edge| edge.kind == GraphEdgeKind::DependsOn)
                .expect("dependency edge should exist")
                .evidence,
            vec!["import_type:Static", "import_type:Dynamic"]
        );
    }

    #[test]
    fn graph_symbol_ids_include_container_name() {
        let workspace_id = WorkspaceId::new("graph-symbol");
        let graph = build_workspace_graph(
            &workspace_id,
            CodeGraphSnapshot {
                files: vec!["src/lib.rs".into()],
                symbols: vec![SymbolEntry {
                    name: "render".into(),
                    kind: SymbolKind::Method,
                    file_path: "src/lib.rs".into(),
                    line: 3,
                    end_line: None,
                    is_exported: false,
                    container: Some("Panel".into()),
                    signature: None,
                }],
                ..CodeGraphSnapshot::default()
            },
            Vec::new(),
            Vec::new(),
            &GraphQuery::default(),
        );

        assert!(
            graph
                .nodes
                .iter()
                .any(|node| node.id == "symbol:src/lib.rs:Panel::render:method")
        );
    }

    #[test]
    fn graph_query_projects_missing_relation_targets_as_dangling() {
        let workspace_id = WorkspaceId::new("graph-dangling");
        let graph = build_workspace_graph(
            &workspace_id,
            CodeGraphSnapshot::default(),
            vec![KnowledgeRecord {
                knowledge_id: "adr-1".into(),
                kind: KnowledgeKind::Adr,
                title: "Decision".into(),
                content: "Content".into(),
                tags: Vec::new(),
                workspace_id: Some(workspace_id.clone()),
                source_ref: None,
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            }],
            vec![KnowledgeRelation {
                relation_id: "relation-1".into(),
                workspace_id: workspace_id.clone(),
                source: crate::graph::GraphNodeRef::Knowledge {
                    knowledge_id: "adr-1".into(),
                },
                kind: GraphEdgeKind::AppliesTo,
                target: crate::graph::GraphNodeRef::File {
                    path: "src/removed.ts".into(),
                },
                origin: GraphEdgeOrigin::ExplicitUser,
                confidence: Some(1.0),
                status: GraphEdgeStatus::Active,
                evidence: vec!["user-selected".into()],
                discovery_key: None,
                discovery_evidence: None,
                reviewed_at: None,
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            }],
            &GraphQuery::default(),
        );

        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.id == "relation-1")
            .expect("explicit relation should be projected");
        assert_eq!(edge.status, GraphEdgeStatus::Dangling);
        assert!(
            graph
                .nodes
                .iter()
                .any(|node| node.id == "file:src/removed.ts"
                    && node.metadata.get("status").map(String::as_str) == Some("dangling"))
        );
    }
}
