use crate::{BuiltinToolAccessMode, ToolExecutionContext, ToolRuntimeResources};
use serde_json::Value;
use std::{path::Path, time::Duration};

use super::{
    failure::{ToolFailure, invalid_input},
    field_bool, field_string, field_string_array, field_usize, parse_json_object,
    required_string_field,
};

/// 代码符号导航：按符号名查定义 / 列出文件符号。基于本地索引引擎的符号表。
pub(super) fn execute_code_symbols(
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> String {
    let request = parse_json_object(input);
    let action = request
        .as_ref()
        .and_then(|obj| field_string(obj, "action"))
        .unwrap_or_default();

    let Some(store) = resources.knowledge_store.as_ref() else {
        return index_unavailable("code_symbols");
    };
    let Some(workspace_id) = context.workspace_id.as_ref() else {
        return workspace_required("code_symbols");
    };

    let symbol_to_json = |s: &magi_knowledge_store::symbol_index::SymbolEntry| {
        serde_json::json!({
            "name": s.name,
            "kind": format!("{:?}", s.kind),
            "path": s.file_path,
            "line": s.line,
            "endLine": s.end_line,
            "exported": s.is_exported,
            "container": s.container,
            "signature": s.signature,
        })
    };

    match action.as_str() {
        "definition" => {
            let Some(name) = request.as_ref().and_then(|obj| field_string(obj, "name")) else {
                return invalid_input("code_symbols", "action=definition 需要 name 字段");
            };
            let limit = request
                .as_ref()
                .and_then(|obj| field_usize(obj, "limit"))
                .unwrap_or(20)
                .clamp(1, 100);
            let Some(symbols) = store.find_symbol_definitions(workspace_id, &name, limit) else {
                return index_not_ready("code_symbols");
            };
            let results: Vec<Value> = symbols.iter().map(symbol_to_json).collect();
            serde_json::json!({
                "tool": "code_symbols",
                "status": "succeeded",
                "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
                "action": "definition",
                "name": name,
                "returned_matches": results.len(),
                "results": results,
                "summary": format!("符号 \"{}\" 找到 {} 处定义", name, results.len())
            })
            .to_string()
        }
        "file_symbols" => {
            let Some(path) = request.as_ref().and_then(|obj| field_string(obj, "path")) else {
                return invalid_input("code_symbols", "action=file_symbols 需要 path 字段");
            };
            let path = normalize_code_symbols_file_path(&path, context);
            let Some(symbols) = store.list_file_symbols(workspace_id, &path) else {
                return index_not_ready("code_symbols");
            };
            let results: Vec<Value> = symbols.iter().map(symbol_to_json).collect();
            serde_json::json!({
                "tool": "code_symbols",
                "status": "succeeded",
                "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
                "action": "file_symbols",
                "path": path,
                "returned_matches": results.len(),
                "results": results,
                "summary": format!("文件 \"{}\" 含 {} 个符号", path, results.len())
            })
            .to_string()
        }
        other => invalid_input(
            "code_symbols",
            format!("未知 action：{other}（支持 definition / file_symbols）"),
        ),
    }
}

pub(super) fn normalize_code_symbols_file_path(
    path: &str,
    context: &ToolExecutionContext,
) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let input_path = Path::new(trimmed);
    let relative = if input_path.is_absolute() {
        context
            .working_directory
            .as_ref()
            .and_then(|workspace_root| {
                input_path
                    .strip_prefix(workspace_root)
                    .ok()
                    .map(Path::to_path_buf)
            })
            .or_else(|| {
                let canonical_input = input_path.canonicalize().ok()?;
                let canonical_root = context.working_directory.as_ref()?.canonicalize().ok()?;
                canonical_input
                    .strip_prefix(canonical_root)
                    .ok()
                    .map(Path::to_path_buf)
            })
            .unwrap_or_else(|| input_path.to_path_buf())
    } else {
        input_path.to_path_buf()
    };

    relative.to_string_lossy().replace('\\', "/")
}

pub(super) fn execute_search_semantic(
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> String {
    let request = parse_json_object(input);
    let query = match required_string_field(
        request.as_ref(),
        "query",
        "search_semantic",
        "缺少 query 字段",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };

    let limit = request
        .as_ref()
        .and_then(|obj| field_usize(obj, "limit"))
        .unwrap_or(10)
        .clamp(1, 50);
    let max_context_tokens = request
        .as_ref()
        .and_then(|obj| field_usize(obj, "max_context_tokens"))
        .unwrap_or(8_000)
        .clamp(256, 32_000);
    let preferred_scopes = request
        .as_ref()
        .map(|obj| field_string_array(obj, "preferred_scopes"))
        .unwrap_or_default();
    let prefer_recent_edits = request
        .as_ref()
        .and_then(|obj| field_bool(obj, "prefer_recent_edits"))
        .unwrap_or(true);

    let Some(store) = resources.knowledge_store.as_ref() else {
        return index_unavailable("search_semantic");
    };
    let Some(workspace_id) = context.workspace_id.as_ref() else {
        return workspace_required("search_semantic");
    };
    let Some(workspace_root) = context.working_directory.as_deref() else {
        return workspace_required("search_semantic");
    };
    if let Err(failure) =
        ensure_workspace_index(store, workspace_id, workspace_root, "search_semantic")
    {
        return failure;
    }
    let Some(engine_results) = store.search_workspace_code(
        workspace_id,
        &query,
        magi_knowledge_store::local_search_engine::SearchOptions {
            max_results: Some(limit),
            max_context_tokens: Some(max_context_tokens),
            preferred_scopes: preferred_scopes.clone(),
            prefer_recent_edits,
        },
    ) else {
        return ToolFailure::new("search_semantic", "index_invalid_state", "代码索引状态异常")
            .instruction("本轮改用 search_text 或 file_read 继续；不要重复调用 search_semantic。")
            .into_payload();
    };

    let results: Vec<Value> = engine_results
        .iter()
        .map(|result| {
            let primary_snippet = result.snippets.first();
            let matched_keywords = primary_snippet
                .map(|snippet| snippet.matched_tokens.clone())
                .unwrap_or_default();
            serde_json::json!({
                "path": &result.file_path,
                "score": result.score,
                "source": "engine",
                "matched_keywords": matched_keywords,
                "snippet": primary_snippet.map(|snippet| snippet.content.as_str()).unwrap_or_default(),
                "snippets": &result.snippets,
                "score_breakdown": &result.score_breakdown,
            })
        })
        .collect();

    serde_json::json!({
        "tool": "search_semantic",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "query": query,
        "engine": "local_search_engine",
        "workspace_id": workspace_id.as_str(),
        "returned_matches": results.len(),
        "max_context_tokens": max_context_tokens,
        "preferred_scopes": preferred_scopes,
        "prefer_recent_edits": prefer_recent_edits,
        "index": store.workspace_index_stats(workspace_id).map(|stats| serde_json::json!({
            "version": stats.index_version,
            "documents": stats.total_documents,
            "symbols": stats.unique_symbols,
            "cache_hit_rate": stats.cache_hit_rate,
            "query_count": stats.query_count,
            "average_query_micros": stats.average_query_micros,
            "max_query_micros": stats.max_query_micros,
        })),
        "results": results,
        "summary": format!("本地代码索引检索 \"{}\" 返回 {} 个匹配", query, results.len())
    })
    .to_string()
}

// ══════════════════════════════════════════════════════════════════════════════
// knowledge.query — 当前工作区知识库检索
// ══════════════════════════════════════════════════════════════════════════════

pub(super) fn execute_knowledge_query(
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> String {
    let request = parse_json_object(input);
    let query = match required_string_field(
        request.as_ref(),
        "query",
        "knowledge_query",
        "缺少 query 字段",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };

    let Some(store) = resources.knowledge_store.as_ref() else {
        return knowledge_store_unavailable("knowledge_query");
    };
    let Some(workspace_id) = context.workspace_id.as_ref() else {
        return workspace_required("knowledge_query");
    };

    let kind = match parse_knowledge_kind(request.as_ref()) {
        Ok(kind) => kind,
        Err(error) => return error,
    };
    let tags = parse_knowledge_tags(request.as_ref());
    let limit = request
        .as_ref()
        .and_then(|obj| field_usize(obj, "limit"))
        .unwrap_or(10)
        .clamp(1, 50);

    let knowledge_query = magi_knowledge_store::KnowledgeQuery {
        kind,
        text: Some(query.clone()),
        tags: tags.clone(),
        workspace_id: Some(workspace_id.clone()),
        limit,
    };
    let governed_query = store.governed_query(&knowledge_query);
    let results: Vec<Value> = governed_query
        .results
        .iter()
        .map(|item| {
            serde_json::json!({
                "knowledge_id": &item.knowledge_id,
                "title": &item.title,
                "kind": knowledge_kind_label(item.kind),
                "excerpt": &item.excerpt,
                "updated_at": item.updated_at,
                "score": item.score,
                "matched_terms": &item.matched_terms,
                "source_ref": item.source_ref.as_deref(),
                "code_source": item.code_source.as_ref(),
                "audit_link": item.audit_link.as_ref(),
                "governance_link": item.governance_link.as_ref(),
            })
        })
        .collect();

    serde_json::json!({
        "tool": "knowledge_query",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "workspace_id": workspace_id.as_str(),
        "query": query,
        "kind": kind.map(knowledge_kind_label).unwrap_or("all"),
        "tags": tags,
        "limit": limit,
        "total_matches": governed_query.total_matches,
        "returned_matches": results.len(),
        "truncated": governed_query.truncated,
        "results": results,
        "summary": format!("在当前工作区知识库中搜索 \"{}\"，返回 {} 个匹配项", query, results.len())
    })
    .to_string()
}

pub(super) fn execute_knowledge_graph_query(
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> String {
    let request = parse_json_object(input);
    let focus = match required_string_field(
        request.as_ref(),
        "focus",
        "knowledge_graph_query",
        "缺少 focus 字段",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let Some(store) = resources.knowledge_store.as_ref() else {
        return knowledge_store_unavailable("knowledge_graph_query");
    };
    let Some(workspace_id) = context.workspace_id.as_ref() else {
        return workspace_required("knowledge_graph_query");
    };
    let Some(workspace_root) = context.working_directory.as_deref() else {
        return workspace_required("knowledge_graph_query");
    };

    if let Err(failure) =
        ensure_workspace_index(store, workspace_id, workspace_root, "knowledge_graph_query")
    {
        return failure;
    }

    let direction = match request
        .as_ref()
        .and_then(|object| field_string(object, "direction"))
    {
        Some(value) => match magi_knowledge_store::GraphDirection::parse(&value) {
            Some(direction) => direction,
            None => {
                return invalid_input(
                    "knowledge_graph_query",
                    "direction 仅支持 forward、reverse 或 both",
                );
            }
        },
        None => magi_knowledge_store::GraphDirection::Both,
    };
    let node_kinds = match parse_graph_node_kinds(request.as_ref()) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let edge_kinds = match parse_graph_edge_kinds(request.as_ref()) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let depth = request
        .as_ref()
        .and_then(|object| field_usize(object, "depth"))
        .unwrap_or(1)
        .min(2);
    let max_nodes = request
        .as_ref()
        .and_then(|object| field_usize(object, "max_nodes"))
        .unwrap_or(40)
        .clamp(1, 80);
    let max_edges = request
        .as_ref()
        .and_then(|object| field_usize(object, "max_edges"))
        .unwrap_or(80)
        .clamp(1, 160);
    let max_context_tokens = request
        .as_ref()
        .and_then(|object| field_usize(object, "max_context_tokens"))
        .unwrap_or(4_000)
        .clamp(256, 8_000);

    let graph_query = magi_knowledge_store::GraphQuery {
        focus: Some(focus.clone()),
        depth,
        direction,
        node_kinds,
        edge_kinds,
        max_nodes,
        max_edges,
    };
    let Some(graph) = store.query_workspace_graph(workspace_id, &graph_query) else {
        return ToolFailure::new(
            "knowledge_graph_query",
            "index_invalid_state",
            "知识图谱索引状态异常",
        )
        .instruction("本轮改用 search_text 或 file_read 继续；不要重复调用 knowledge_graph_query。")
        .into_payload();
    };
    let graph = limit_graph_context(graph, &focus, max_context_tokens);
    let candidate_edges = graph
        .edges
        .iter()
        .filter(|edge| edge.status == magi_knowledge_store::GraphEdgeStatus::Candidate)
        .count();
    let determined_edges = graph
        .edges
        .iter()
        .filter(|edge| {
            edge.status == magi_knowledge_store::GraphEdgeStatus::Active
                && edge.origin != magi_knowledge_store::GraphEdgeOrigin::Inferred
        })
        .count();
    let returned_nodes = graph.stats.returned_nodes;
    let returned_edges = graph.stats.returned_edges;

    serde_json::json!({
        "tool": "knowledge_graph_query",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "workspace_id": workspace_id.as_str(),
        "focus": focus,
        "depth": depth,
        "direction": direction,
        "candidate_edges": candidate_edges,
        "determined_edges": determined_edges,
        "nodes": graph.nodes,
        "edges": graph.edges,
        "stats": graph.stats,
        "truncated": graph.truncated,
        "summary": format!("读取焦点 {} 的知识图谱邻居，返回 {} 个节点和 {} 条关系", focus, returned_nodes, returned_edges)
    })
    .to_string()
}

pub(super) fn parse_graph_node_kinds(
    request: Option<&serde_json::Map<String, Value>>,
) -> Result<Vec<magi_knowledge_store::GraphNodeKind>, String> {
    parse_graph_kind_array(
        request,
        "node_kinds",
        magi_knowledge_store::GraphNodeKind::parse,
        "node_kinds",
    )
}

pub(super) fn parse_graph_edge_kinds(
    request: Option<&serde_json::Map<String, Value>>,
) -> Result<Vec<magi_knowledge_store::GraphEdgeKind>, String> {
    parse_graph_kind_array(
        request,
        "edge_kinds",
        magi_knowledge_store::GraphEdgeKind::parse,
        "edge_kinds",
    )
}

pub(super) fn parse_graph_kind_array<T>(
    request: Option<&serde_json::Map<String, Value>>,
    field: &str,
    parse: fn(&str) -> Option<T>,
    label: &str,
) -> Result<Vec<T>, String>
where
    T: PartialEq,
{
    let Some(values) = request.and_then(|object| object.get(field)) else {
        return Ok(Vec::new());
    };
    let Some(values) = values.as_array() else {
        return Err(invalid_input(
            "knowledge_graph_query",
            format!("{label} 必须是字符串数组"),
        ));
    };
    let mut result = Vec::new();
    for value in values {
        let Some(value) = value.as_str() else {
            return Err(invalid_input(
                "knowledge_graph_query",
                format!("{label} 必须只包含字符串"),
            ));
        };
        let Some(parsed) = parse(value) else {
            return Err(invalid_input(
                "knowledge_graph_query",
                format!("{label} 包含不支持的值：{value}"),
            ));
        };
        if !result.iter().any(|item| item == &parsed) {
            result.push(parsed);
        }
    }
    Ok(result)
}

pub(super) fn limit_graph_context(
    mut graph: magi_knowledge_store::KnowledgeGraph,
    focus: &str,
    max_context_tokens: usize,
) -> magi_knowledge_store::KnowledgeGraph {
    let original_nodes = graph.nodes.len();
    let original_edges = graph.edges.len();
    let mut ordered_nodes = Vec::with_capacity(original_nodes);
    if let Some(index) = graph.nodes.iter().position(|node| node.id == focus) {
        ordered_nodes.push(graph.nodes[index].clone());
    }
    ordered_nodes.extend(graph.nodes.iter().filter(|node| node.id != focus).cloned());

    let mut used_tokens = 0usize;
    let mut kept_nodes = Vec::new();
    for node in ordered_nodes {
        let cost = estimated_json_tokens(&node);
        if !kept_nodes.is_empty() && used_tokens.saturating_add(cost) > max_context_tokens {
            break;
        }
        used_tokens = used_tokens.saturating_add(cost);
        kept_nodes.push(node);
    }
    let kept_ids = kept_nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut kept_edges = Vec::new();
    for edge in graph.edges {
        if !kept_ids.contains(edge.source.as_str()) || !kept_ids.contains(edge.target.as_str()) {
            continue;
        }
        let cost = estimated_json_tokens(&edge);
        if !kept_edges.is_empty() && used_tokens.saturating_add(cost) > max_context_tokens {
            break;
        }
        used_tokens = used_tokens.saturating_add(cost);
        kept_edges.push(edge);
    }
    graph.nodes = kept_nodes;
    graph.edges = kept_edges;
    graph.stats.returned_nodes = graph.nodes.len();
    graph.stats.returned_edges = graph.edges.len();
    graph.truncated = graph.truncated
        || original_nodes != graph.nodes.len()
        || original_edges != graph.edges.len();
    graph
}

pub(super) fn estimated_json_tokens<T: serde::Serialize>(value: &T) -> usize {
    serde_json::to_string(value)
        .map(|value| (value.len() / 4).max(1))
        .unwrap_or(64)
}

pub(super) fn parse_knowledge_kind(
    request: Option<&serde_json::Map<String, Value>>,
) -> Result<Option<magi_knowledge_store::KnowledgeKind>, String> {
    let Some(kind) = request.and_then(|obj| field_string(obj, "kind")) else {
        return Ok(None);
    };
    match kind.as_str() {
        "all" => Ok(None),
        "adr" => Ok(Some(magi_knowledge_store::KnowledgeKind::Adr)),
        "faq" => Ok(Some(magi_knowledge_store::KnowledgeKind::Faq)),
        "learning" => Ok(Some(magi_knowledge_store::KnowledgeKind::Learning)),
        "code_index" => Ok(Some(magi_knowledge_store::KnowledgeKind::CodeIndex)),
        other => Err(invalid_input(
            "knowledge_query",
            format!("未知 kind：{other}（支持 all / adr / faq / learning / code_index）"),
        )),
    }
}

pub(super) fn parse_knowledge_tags(
    request: Option<&serde_json::Map<String, Value>>,
) -> Vec<String> {
    request
        .map(|obj| field_string_array(obj, "tags"))
        .unwrap_or_default()
}

/// 索引、知识库、工作区这类“当前不可用”的失败共用的措辞：都不是重试能解决的，
/// 模型应当换用不依赖索引的工具继续。
fn index_unavailable(tool: &str) -> String {
    ToolFailure::new(tool, "index_unavailable", "代码索引引擎不可用")
        .instruction("本轮改用 search_text 或 file_read 继续；不要重复调用该工具。")
        .into_payload()
}

fn index_not_ready(tool: &str) -> String {
    ToolFailure::new(tool, "index_not_ready", "代码索引尚未就绪")
        .instruction(
            "本轮改用 search_text 或 file_read 继续；索引在后台构建，不要立即重复调用该工具。",
        )
        .into_payload()
}

fn knowledge_store_unavailable(tool: &str) -> String {
    ToolFailure::new(tool, "knowledge_unavailable", "知识库不可用")
        .instruction("本轮改用 search_text 或 file_read 查找信息；不要重复调用该工具。")
        .into_payload()
}

fn workspace_required(tool: &str) -> String {
    ToolFailure::new(
        tool,
        "workspace_required",
        "当前会话没有可用的工作区，无法使用该工具",
    )
    .instruction("改用不依赖工作区索引的工具，或告知用户先选择工作区。")
    .into_payload()
}

/// 确保工作区代码索引可用：等待后台构建一小段时间，超时或失败按类别返回。
fn ensure_workspace_index(
    store: &magi_knowledge_store::KnowledgeStore,
    workspace_id: &magi_core::WorkspaceId,
    workspace_root: &std::path::Path,
    tool: &str,
) -> Result<(), String> {
    use magi_knowledge_store::{WorkspaceIndexEnsureResult, code_scanner::CodeIndexScanReasonCode};
    match store.ensure_workspace_index_available(
        workspace_id,
        workspace_root,
        Duration::from_secs(15),
    ) {
        WorkspaceIndexEnsureResult::Ready => Ok(()),
        WorkspaceIndexEnsureResult::TimedOut => Err(ToolFailure::new(
            tool,
            "index_building",
            "代码索引仍在后台构建，暂时不能使用",
        )
        .instruction(
            "本轮改用 search_text 或 file_read 继续；不要立即重复调用该工具，完成其他步骤后再试。",
        )
        .into_payload()),
        WorkspaceIndexEnsureResult::Failed { reason_code } => {
            let reason = match reason_code {
                Some(CodeIndexScanReasonCode::WorkspaceMissing) => {
                    "工作区目录不存在，无法构建代码索引"
                }
                Some(CodeIndexScanReasonCode::WorkspaceNotDirectory) => {
                    "工作区路径不是目录，无法构建代码索引"
                }
                Some(CodeIndexScanReasonCode::WorkspaceUnreadable) => {
                    "工作区目录不可读取，无法构建代码索引"
                }
                Some(CodeIndexScanReasonCode::NoIndexableFiles) => "工作区中没有可索引文件",
                None => "代码索引构建失败",
            };
            Err(ToolFailure::new(tool, "index_failed", reason)
                .instruction("本轮改用 search_text 或 file_read 继续；不要重复调用该工具。")
                .into_payload())
        }
    }
}

pub(super) fn knowledge_kind_label(kind: magi_knowledge_store::KnowledgeKind) -> &'static str {
    match kind {
        magi_knowledge_store::KnowledgeKind::Adr => "adr",
        magi_knowledge_store::KnowledgeKind::Faq => "faq",
        magi_knowledge_store::KnowledgeKind::Learning => "learning",
        magi_knowledge_store::KnowledgeKind::CodeIndex => "code_index",
    }
}
