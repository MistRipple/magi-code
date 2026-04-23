use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicroCompactionStats {
    pub compacted_count: u32,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub tokens_saved: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicroCompactionMode {
    Compact,
    Clear,
}

impl Default for MicroCompactionMode {
    fn default() -> Self {
        Self::Compact
    }
}

#[derive(Clone, Debug)]
pub struct LlmMessage {
    pub role: String,
    pub content: LlmContent,
}

#[derive(Clone, Debug)]
pub enum LlmContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Clone, Debug)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        media_type: String,
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
        tool_name: Option<String>,
        status: Option<String>,
    },
}

pub fn estimate_token_count(text: &str) -> u64 {
    let chars = text.len() as u64;
    // ~4 chars per token (rough estimate)
    chars / 4 + 1
}

pub fn compact_old_tool_results(
    history: &mut [LlmMessage],
    preserve_last_n: usize,
    min_content_length: usize,
    mode: MicroCompactionMode,
) -> MicroCompactionStats {
    let mut stats = MicroCompactionStats::default();

    let tool_result_user_indices: Vec<usize> = history
        .iter()
        .enumerate()
        .filter(|(_, msg)| {
            msg.role == "user"
                && matches!(&msg.content, LlmContent::Blocks(blocks) if blocks.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. })))
        })
        .map(|(i, _)| i)
        .collect();

    let compactable_count = tool_result_user_indices
        .len()
        .saturating_sub(preserve_last_n);
    let compactable_indices = &tool_result_user_indices[..compactable_count];

    if compactable_indices.is_empty() {
        return stats;
    }

    for &idx in compactable_indices {
        if let LlmContent::Blocks(blocks) = &mut history[idx].content {
            for block in blocks.iter_mut() {
                if let ContentBlock::ToolResult {
                    content,
                    is_error,
                    tool_name,
                    status,
                    ..
                } = block
                {
                    if mode != MicroCompactionMode::Clear && content.len() <= min_content_length {
                        continue;
                    }

                    let tokens_before = estimate_token_count(content);
                    stats.tokens_before += tokens_before;

                    let resolved_tool_name = tool_name.as_deref().unwrap_or("unknown");
                    let resolved_status = if *is_error {
                        "error"
                    } else {
                        status.as_deref().unwrap_or("success")
                    };

                    let compacted =
                        build_compact_summary(resolved_tool_name, resolved_status, content, mode);
                    *content = compacted;
                    stats.compacted_count += 1;

                    let tokens_after = estimate_token_count(content);
                    stats.tokens_after += tokens_after;
                }
            }
        }
    }

    stats.tokens_saved = stats.tokens_before.saturating_sub(stats.tokens_after);
    stats
}

fn build_compact_summary(
    tool_name: &str,
    status: &str,
    original_content: &str,
    mode: MicroCompactionMode,
) -> String {
    let first_line = original_content
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(120)
        .collect::<String>();
    let char_count = original_content.len();
    let line_count = original_content.lines().count();

    if mode == MicroCompactionMode::Clear {
        return format!(
            "[Cleared tool result after idle compaction for {}: {}, {} chars, {} lines]",
            tool_name, status, char_count, line_count,
        );
    }

    let mut summary = format!(
        "[Compacted tool result for {}: {}, {} chars, {} lines]",
        tool_name, status, char_count, line_count,
    );
    if !first_line.is_empty() {
        let ellipsis = if first_line.len() >= 120 { "..." } else { "" };
        summary.push_str(&format!("\n> {}{}", first_line, ellipsis));
    }

    summary
}
