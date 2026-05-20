use std::collections::HashSet;

const MAX_CONCURRENT: usize = 10;

#[derive(Clone, Copy, Debug)]
pub struct ToolConcurrencyInput<'a> {
    pub tool_name: &'a str,
    pub arguments: Option<&'a serde_json::Value>,
}

fn concurrent_safe_tools() -> HashSet<&'static str> {
    [
        "file_view",
        "code_search_regex",
        "web_search",
        "web_fetch",
        "diagram_render",
        "code_search_semantic",
        "project_knowledge_query",
    ]
    .into_iter()
    .collect()
}

pub fn is_concurrency_safe(tool_name: &str) -> bool {
    is_concurrency_safe_call(&ToolConcurrencyInput {
        tool_name,
        arguments: None,
    })
}

pub fn is_concurrency_safe_call(input: &ToolConcurrencyInput<'_>) -> bool {
    if is_shell_like_tool(input.tool_name) {
        return input
            .arguments
            .and_then(read_access_mode)
            .is_some_and(|mode| mode == "read_only" || mode == "readonly" || mode == "read-only");
    }
    concurrent_safe_tools().contains(input.tool_name)
}

fn is_shell_like_tool(tool_name: &str) -> bool {
    tool_name == "shell_exec"
}

fn read_access_mode(arguments: &serde_json::Value) -> Option<String> {
    let object = arguments.as_object()?;
    ["access_mode", "write_mode", "intent"]
        .iter()
        .find_map(|key| object.get(*key).and_then(serde_json::Value::as_str))
        .map(|value| value.trim().to_ascii_lowercase())
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
    let inputs = tool_names
        .iter()
        .map(|tool_name| ToolConcurrencyInput {
            tool_name,
            arguments: None,
        })
        .collect::<Vec<_>>();
    partition_tool_calls_with_inputs(&inputs)
}

pub fn partition_tool_calls_with_inputs(tool_calls: &[ToolConcurrencyInput<'_>]) -> Vec<ToolBatch> {
    if tool_calls.is_empty() {
        return Vec::new();
    }
    if tool_calls.len() == 1 {
        return vec![ToolBatch {
            kind: if is_concurrency_safe_call(&tool_calls[0]) {
                ToolBatchKind::Concurrent
            } else {
                ToolBatchKind::Serial
            },
            tool_indices: vec![0],
        }];
    }

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

    for (i, tool_call) in tool_calls.iter().enumerate() {
        if is_concurrency_safe_call(tool_call) {
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
