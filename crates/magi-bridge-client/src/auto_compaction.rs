use crate::micro_compaction::{estimate_token_count, LlmContent, LlmMessage};

const AUTOCOMPACT_BUFFER_TOKENS: u64 = 13_000;
const MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES: u32 = 3;
const SM_COMPACT_MIN_TEXT_BLOCK_MESSAGES: u32 = 5;
const SM_COMPACT_MIN_TOKENS: u64 = 10_000;
const SM_COMPACT_MAX_TOKENS: u64 = 40_000;

#[derive(Clone, Debug)]
pub struct AutoCompactConfig {
    pub context_window_tokens: u64,
    pub custom_threshold: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct AutoCompactResult {
    pub was_compacted: bool,
    pub method: Option<CompactionMethod>,
    pub pre_compact_tokens: Option<u64>,
    pub post_compact_tokens: Option<u64>,
    pub retained_message_count: Option<usize>,
    pub generated_summary: Option<String>,
    pub error: Option<String>,
}

impl AutoCompactResult {
    fn not_compacted() -> Self {
        Self {
            was_compacted: false,
            method: None,
            pre_compact_tokens: None,
            post_compact_tokens: None,
            retained_message_count: None,
            generated_summary: None,
            error: None,
        }
    }

    fn with_error(error: impl Into<String>) -> Self {
        Self {
            was_compacted: false,
            error: Some(error.into()),
            ..Self::not_compacted()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionMethod {
    SessionMemory,
    FullSummary,
    Truncation,
}

#[derive(Clone, Debug)]
pub struct CompactionTrackingState {
    pub compacted: bool,
    pub turn_counter: u32,
    pub consecutive_failures: u32,
}

impl CompactionTrackingState {
    pub fn new() -> Self {
        Self {
            compacted: false,
            turn_counter: 0,
            consecutive_failures: 0,
        }
    }
}

impl Default for CompactionTrackingState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
pub struct SessionMemoryContent {
    pub summary: String,
    pub token_estimate: u64,
}

pub fn get_auto_compact_threshold(config: &AutoCompactConfig) -> u64 {
    if let Some(custom) = config.custom_threshold {
        if custom > 0 {
            return custom.min(
                config
                    .context_window_tokens
                    .saturating_sub(AUTOCOMPACT_BUFFER_TOKENS),
            );
        }
    }
    config
        .context_window_tokens
        .saturating_sub(AUTOCOMPACT_BUFFER_TOKENS)
}

pub fn estimate_history_tokens(history: &[LlmMessage]) -> u64 {
    let mut total = 0u64;
    for msg in history {
        match &msg.content {
            LlmContent::Text(text) => {
                total += estimate_token_count(text);
            }
            LlmContent::Blocks(blocks) => {
                for block in blocks {
                    match block {
                        crate::micro_compaction::ContentBlock::Text { text } => {
                            total += estimate_token_count(text);
                        }
                        crate::micro_compaction::ContentBlock::ToolResult { content, .. } => {
                            total += estimate_token_count(content);
                        }
                        _ => {
                            total += 50;
                        }
                    }
                }
            }
        }
    }
    total
}

pub fn should_auto_compact(
    history: &[LlmMessage],
    config: &AutoCompactConfig,
    tracking: Option<&CompactionTrackingState>,
) -> bool {
    if let Some(t) = tracking {
        if t.consecutive_failures >= MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES {
            return false;
        }
    }
    let token_count = estimate_history_tokens(history);
    let threshold = get_auto_compact_threshold(config);
    token_count >= threshold
}

pub fn try_session_memory_compaction(
    history: &mut Vec<LlmMessage>,
    session_memory: Option<&SessionMemoryContent>,
    config: &AutoCompactConfig,
) -> AutoCompactResult {
    let sm = match session_memory {
        Some(sm) if !sm.summary.trim().is_empty() => sm,
        _ => return AutoCompactResult::with_error("no session memory available"),
    };

    let threshold = get_auto_compact_threshold(config);
    let pre_compact_tokens = estimate_history_tokens(history);

    let target_tokens = SM_COMPACT_MAX_TOKENS
        .min(SM_COMPACT_MIN_TOKENS.max(threshold / 2));

    let session_memory_tokens = estimate_token_count(&sm.summary);
    let available_for_messages = target_tokens.saturating_sub(session_memory_tokens);

    let mut retained_tokens = 0u64;
    let mut retained_count = 0usize;
    let mut text_block_message_count = 0u32;

    for i in (0..history.len()).rev() {
        let msg = &history[i];
        let msg_tokens = match &msg.content {
            LlmContent::Text(text) => estimate_token_count(text),
            LlmContent::Blocks(_) => 50,
        };

        if retained_tokens + msg_tokens > available_for_messages
            && text_block_message_count >= SM_COMPACT_MIN_TEXT_BLOCK_MESSAGES
        {
            break;
        }

        retained_tokens += msg_tokens;
        retained_count += 1;

        if let LlmContent::Text(text) = &msg.content {
            if text.trim().len() > 20 {
                text_block_message_count += 1;
            }
        }
    }

    if retained_count == 0 {
        return AutoCompactResult::with_error("no messages to retain");
    }

    let drop_count = history.len().saturating_sub(retained_count);
    if drop_count == 0 {
        return AutoCompactResult::with_error("nothing to compact");
    }

    let retained_messages: Vec<LlmMessage> =
        history.split_off(history.len() - retained_count);

    let summary_message = LlmMessage {
        role: "user".to_string(),
        content: LlmContent::Text(format!(
            "[Session Memory — 以下是之前对话的核心记忆，共裁剪 {} 条历史消息]\n\n{}",
            drop_count, sm.summary,
        )),
    };

    history.clear();
    history.push(summary_message);
    history.extend(retained_messages);

    let post_compact_tokens = estimate_history_tokens(history);

    AutoCompactResult {
        was_compacted: true,
        method: Some(CompactionMethod::SessionMemory),
        pre_compact_tokens: Some(pre_compact_tokens),
        post_compact_tokens: Some(post_compact_tokens),
        retained_message_count: Some(retained_count + 1),
        generated_summary: None,
        error: None,
    }
}

pub fn force_history_truncation(
    history: &mut Vec<LlmMessage>,
    config: &AutoCompactConfig,
) -> AutoCompactResult {
    let target_tokens = config.context_window_tokens / 2;
    let pre_compact_tokens = estimate_history_tokens(history);

    let mut retained_tokens = 0u64;
    let mut retained_count = 0usize;

    for i in (0..history.len()).rev() {
        let msg = &history[i];
        let msg_tokens = match &msg.content {
            LlmContent::Text(text) => estimate_token_count(text),
            LlmContent::Blocks(_) => 100,
        };

        if retained_tokens + msg_tokens > target_tokens && retained_count >= 2 {
            break;
        }
        retained_tokens += msg_tokens;
        retained_count += 1;
    }

    if retained_count >= history.len() {
        return AutoCompactResult::not_compacted();
    }

    let drop_count = history.len() - retained_count;
    let retained = history.split_off(history.len() - retained_count);

    let truncation_notice = LlmMessage {
        role: "user".to_string(),
        content: LlmContent::Text(format!(
            "[System] 对话历史过长，已自动截断 {} 条旧消息以保持在上下文窗口内。",
            drop_count,
        )),
    };

    history.clear();
    history.push(truncation_notice);
    history.extend(retained);

    let post_compact_tokens = estimate_history_tokens(history);

    AutoCompactResult {
        was_compacted: true,
        method: Some(CompactionMethod::Truncation),
        pre_compact_tokens: Some(pre_compact_tokens),
        post_compact_tokens: Some(post_compact_tokens),
        retained_message_count: Some(retained_count + 1),
        generated_summary: None,
        error: None,
    }
}

pub fn extract_summary_content(raw: &str) -> String {
    if let Some(start) = raw.find("<summary>") {
        if let Some(end) = raw.find("</summary>") {
            let inner = &raw[start + "<summary>".len()..end];
            let trimmed = inner.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    let without_analysis = raw
        .split("<analysis>")
        .flat_map(|part| {
            if let Some(after) = part.find("</analysis>") {
                Some(&part[after + "</analysis>".len()..])
            } else {
                Some(part)
            }
        })
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string();

    if without_analysis.is_empty() {
        raw.to_string()
    } else {
        without_analysis
    }
}
