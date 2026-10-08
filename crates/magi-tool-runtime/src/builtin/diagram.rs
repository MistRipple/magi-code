use crate::BuiltinToolAccessMode;
use serde_json::Value;

use super::{
    failure::{ToolFailure, invalid_input},
    field_bool, field_string, parse_json_object,
};

pub(super) fn execute_diagram_render(input: &str) -> String {
    let request = match parse_json_object(input) {
        Some(obj) => obj,
        None => return invalid_input("diagram_render", "输入必须为 JSON 对象"),
    };

    let kind = match field_string(&request, "kind") {
        Some(value) => normalize_diagram_kind(&value),
        None => None,
    };
    let kind = match kind {
        Some(value) => value,
        None => {
            return invalid_input(
                "diagram_render",
                "缺少或不支持的图表 kind，支持 mermaid、dot、graph、flow",
            );
        }
    };

    let title = field_string(&request, "title");
    let theme = field_string(&request, "theme").unwrap_or_else(|| "default".to_string());
    let layout = field_string(&request, "layout")
        .and_then(|value| normalize_diagram_layout(&value))
        .unwrap_or_else(|| "auto".to_string());
    let interactive =
        field_bool(&request, "interactive").unwrap_or(matches!(kind, "graph" | "flow"));

    let mut payload = serde_json::json!({
        "tool": "diagram_render",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "type": "diagram_render",
        "kind": kind,
        "title": title,
        "theme": theme,
        "layout": layout,
        "interactive": interactive,
    });

    match kind {
        "mermaid" => {
            let source = match field_string(&request, "source") {
                Some(value) if !value.trim().is_empty() => value.trim().to_string(),
                _ => return invalid_input("diagram_render", "该图表源码格式需要 source 字段"),
            };
            let diagram_type = match detect_mermaid_kind_type(&source) {
                Some(value) => value,
                None => {
                    return ToolFailure::new(
                        "diagram_render",
                        "unrecognized_diagram",
                        "无法识别的 Mermaid 图表类型",
                    )
                    .instruction("source 须以有效声明开头（graph、flowchart、sequenceDiagram、classDiagram 等），修正后重新调用。")
                    .into_payload();
                }
            };
            if diagram_type == "mindmap" {
                return ToolFailure::new(
                    "diagram_render",
                    "mindmap_unsupported",
                    "当前产品不展示 Mermaid mindmap",
                )
                .instruction(
                    "思维导图改用 kind=flow 或 kind=graph，并用 graph.nodes/edges 结构化输入。",
                )
                .into_payload();
            }
            payload["source"] = serde_json::json!(source);
            payload["diagram_type"] = serde_json::json!(diagram_type);
            payload["summary"] = serde_json::json!(format!("已生成图表数据（{}）", diagram_type));
        }
        "dot" => {
            let source = match field_string(&request, "source") {
                Some(value) if !value.trim().is_empty() => value.trim().to_string(),
                _ => return invalid_input("diagram_render", "该图表源码格式需要 source 字段"),
            };
            if !is_dot_source(&source) {
                return ToolFailure::new("diagram_render", "invalid_dot_source", "DOT 源码开头不合法")
                    .instruction("DOT 源码须以 graph、digraph、strict graph 或 strict digraph 开头，修正后重新调用。")
                    .into_payload();
            }
            payload["source"] = serde_json::json!(source);
            payload["diagram_type"] = serde_json::json!("dot");
            payload["summary"] = serde_json::json!("已生成图表数据");
        }
        "graph" | "flow" => {
            let graph = match request.get("graph") {
                Some(value) if validate_graph_payload(value) => value.clone(),
                _ => {
                    return invalid_input(
                        "diagram_render",
                        format!("kind={kind} 需要 graph.nodes 和 graph.edges 数组"),
                    );
                }
            };
            let node_count = graph
                .get("nodes")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let edge_count = graph
                .get("edges")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            payload["graph"] = graph;
            payload["diagram_type"] = serde_json::json!(kind);
            payload["summary"] = serde_json::json!(format!(
                "已生成图表数据（{} 个节点，{} 条边）",
                node_count, edge_count
            ));
        }
        _ => unreachable!("normalize_diagram_kind only returns supported kinds"),
    }

    payload.to_string()
}

pub(super) fn normalize_diagram_kind(value: &str) -> Option<&'static str> {
    match value {
        "mermaid" => Some("mermaid"),
        "dot" => Some("dot"),
        "graph" => Some("graph"),
        "flow" => Some("flow"),
        _ => None,
    }
}

pub(super) fn normalize_diagram_layout(value: &str) -> Option<String> {
    match value {
        "auto" | "dagre" | "elk" | "tidy-tree" | "cose" | "force" | "fcose" | "cose-bilkent"
        | "grid" | "circle" | "preset" => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn detect_mermaid_kind_type(source: &str) -> Option<&'static str> {
    let trimmed = strip_mermaid_frontmatter(source).trim_start();
    let diagram_types: &[(&str, &str)] = &[
        ("graph ", "flowchart"),
        ("flowchart ", "flowchart"),
        ("sequenceDiagram", "sequence"),
        ("classDiagram", "class"),
        ("stateDiagram", "state"),
        ("erDiagram", "er"),
        ("gantt", "gantt"),
        ("pie", "pie"),
        ("journey", "journey"),
        ("gitGraph", "git"),
        ("mindmap", "mindmap"),
        ("timeline", "timeline"),
        ("quadrantChart", "quadrant"),
        ("requirementDiagram", "requirement"),
        ("C4Context", "c4"),
        ("sankey", "sankey"),
        ("xychart", "xychart"),
        ("block-beta", "block"),
    ];

    diagram_types
        .iter()
        .find(|(prefix, _)| trimmed.to_lowercase().starts_with(&prefix.to_lowercase()))
        .map(|(_, diagram_type)| *diagram_type)
}

pub(super) fn strip_mermaid_frontmatter(source: &str) -> &str {
    let trimmed = source.trim_start();
    let Some(after_open) = trimmed.strip_prefix("---") else {
        return source;
    };
    let after_open = after_open.trim_start_matches(['\r', '\n']);
    if let Some(close_index) = after_open.find("\n---") {
        let after_close = &after_open[close_index + "\n---".len()..];
        return after_close.trim_start_matches(['\r', '\n']);
    }
    source
}

pub(super) fn is_dot_source(source: &str) -> bool {
    let lower = source.trim_start().to_ascii_lowercase();
    lower.starts_with("graph ")
        || lower.starts_with("graph{")
        || lower.starts_with("digraph ")
        || lower.starts_with("digraph{")
        || lower.starts_with("strict graph ")
        || lower.starts_with("strict graph{")
        || lower.starts_with("strict digraph ")
        || lower.starts_with("strict digraph{")
}

pub(super) fn validate_graph_payload(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.get("nodes").and_then(Value::as_array).is_some()
        && object.get("edges").and_then(Value::as_array).is_some()
}
