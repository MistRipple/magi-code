use std::collections::HashSet;

const MAX_CONCURRENT: usize = 10;

fn concurrent_safe_tools() -> HashSet<&'static str> {
    [
        "file_view",
        "code_search_regex",
        "web_search",
        "web_fetch",
        "mermaid_diagram",
        "code_search_semantic",
        "process_read",
        "process_list",
        "todo_list",
        "project_knowledge_query",
    ]
    .into_iter()
    .collect()
}

pub fn is_concurrency_safe(tool_name: &str) -> bool {
    concurrent_safe_tools().contains(tool_name)
}

#[derive(Clone, Debug)]
pub enum ToolBatchKind {
    Concurrent,
    Serial,
}

#[derive(Clone, Debug)]
pub struct ToolBatch {
    pub kind: ToolBatchKind,
    pub tool_indices: Vec<usize>,
}

pub fn partition_tool_calls(tool_names: &[&str]) -> Vec<ToolBatch> {
    if tool_names.is_empty() {
        return Vec::new();
    }
    if tool_names.len() == 1 {
        return vec![ToolBatch {
            kind: if is_concurrency_safe(tool_names[0]) {
                ToolBatchKind::Concurrent
            } else {
                ToolBatchKind::Serial
            },
            tool_indices: vec![0],
        }];
    }

    let safe_set = concurrent_safe_tools();
    let mut batches = Vec::new();
    let mut current_read_only: Vec<usize> = Vec::new();

    let flush_read_only = |batches: &mut Vec<ToolBatch>, buf: &mut Vec<usize>| {
        if buf.is_empty() {
            return;
        }
        for chunk in buf.chunks(MAX_CONCURRENT) {
            batches.push(ToolBatch {
                kind: ToolBatchKind::Concurrent,
                tool_indices: chunk.to_vec(),
            });
        }
        buf.clear();
    };

    for (i, name) in tool_names.iter().enumerate() {
        if safe_set.contains(name) {
            current_read_only.push(i);
        } else {
            flush_read_only(&mut batches, &mut current_read_only);
            batches.push(ToolBatch {
                kind: ToolBatchKind::Serial,
                tool_indices: vec![i],
            });
        }
    }
    flush_read_only(&mut batches, &mut current_read_only);

    batches
}
