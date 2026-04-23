use std::sync::Arc;

use crate::llm_types::{LlmContentBlock, LlmMessage, LlmMessageContent};
use crate::types::{ModelBridgeClient, ModelInvocationRequest};

#[derive(Clone, Debug)]
pub struct ConversationCompactionConfig {
    pub trigger_threshold_tokens: u64,
    pub target_tokens: u64,
    pub protect_recent_count: usize,
    pub pin_first_message: bool,
}

impl Default for ConversationCompactionConfig {
    fn default() -> Self {
        Self {
            trigger_threshold_tokens: 20_000,
            target_tokens: 6_000,
            protect_recent_count: 8,
            pin_first_message: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompactionResult {
    pub original_message_count: usize,
    pub compacted_message_count: usize,
    pub estimated_original_tokens: u64,
    pub estimated_compacted_tokens: u64,
    pub method: CompactionMethod,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionMethod {
    LlmSummary,
    RuleTruncation,
    NoAction,
}

pub struct ConversationCompactor {
    config: ConversationCompactionConfig,
    model_client: Option<Arc<dyn ModelBridgeClient>>,
}

impl ConversationCompactor {
    pub fn new(config: ConversationCompactionConfig) -> Self {
        Self {
            config,
            model_client: None,
        }
    }

    pub fn with_model_client(mut self, client: Arc<dyn ModelBridgeClient>) -> Self {
        self.model_client = Some(client);
        self
    }

    pub fn should_compact(&self, messages: &[LlmMessage]) -> bool {
        let estimated = crate::protocol::estimate_message_tokens(messages);
        estimated > self.config.trigger_threshold_tokens
    }

    pub fn compact(&self, messages: &[LlmMessage]) -> (Vec<LlmMessage>, CompactionResult) {
        let original_count = messages.len();
        let estimated_tokens = crate::protocol::estimate_message_tokens(messages);

        if estimated_tokens <= self.config.trigger_threshold_tokens {
            return (
                messages.to_vec(),
                CompactionResult {
                    original_message_count: original_count,
                    compacted_message_count: original_count,
                    estimated_original_tokens: estimated_tokens,
                    estimated_compacted_tokens: estimated_tokens,
                    method: CompactionMethod::NoAction,
                },
            );
        }

        if let Some(ref client) = self.model_client {
            if let Some(result) = self.try_llm_compaction(messages, client) {
                return result;
            }
        }

        self.rule_truncation(messages)
    }

    fn try_llm_compaction(
        &self,
        messages: &[LlmMessage],
        client: &Arc<dyn ModelBridgeClient>,
    ) -> Option<(Vec<LlmMessage>, CompactionResult)> {
        let original_count = messages.len();
        let estimated_tokens = crate::protocol::estimate_message_tokens(messages);

        let (pinned, compactable, protected) = self.split_messages(messages);

        if compactable.is_empty() {
            return None;
        }

        let summary_text = self.build_compaction_summary(&compactable);
        let prompt = format!(
            "请将以下对话历史压缩为一段简洁的摘要，保留关键决策、工具调用结果和重要上下文。\
            目标长度：约 {} tokens。\n\n---\n\n{}",
            self.config.target_tokens, summary_text
        );

        let request = ModelInvocationRequest {
            provider: "openai".to_string(),
            prompt,
            messages: None,
            tools: None,
            tool_choice: None,
        };

        let response = client.invoke(request).ok()?;
        let compacted_text = response.payload;

        let mut result_messages = pinned;
        result_messages.push(LlmMessage {
            role: "assistant".to_string(),
            content: LlmMessageContent::Text(format!("[对话历史压缩摘要]\n{}", compacted_text)),
        });
        result_messages.extend(protected);

        let compacted_tokens = crate::protocol::estimate_message_tokens(&result_messages);
        let compacted_count = result_messages.len();

        Some((
            result_messages,
            CompactionResult {
                original_message_count: original_count,
                compacted_message_count: compacted_count,
                estimated_original_tokens: estimated_tokens,
                estimated_compacted_tokens: compacted_tokens,
                method: CompactionMethod::LlmSummary,
            },
        ))
    }

    fn rule_truncation(&self, messages: &[LlmMessage]) -> (Vec<LlmMessage>, CompactionResult) {
        let original_count = messages.len();
        let estimated_tokens = crate::protocol::estimate_message_tokens(messages);

        let (pinned, _compactable, protected) = self.split_messages(messages);

        let mut result = pinned;
        result.push(LlmMessage {
            role: "assistant".to_string(),
            content: LlmMessageContent::Text("[对话历史已截断以适应上下文窗口限制]".to_string()),
        });
        result.extend(protected);

        let compacted_tokens = crate::protocol::estimate_message_tokens(&result);
        let compacted_count = result.len();

        (
            result,
            CompactionResult {
                original_message_count: original_count,
                compacted_message_count: compacted_count,
                estimated_original_tokens: estimated_tokens,
                estimated_compacted_tokens: compacted_tokens,
                method: CompactionMethod::RuleTruncation,
            },
        )
    }

    fn split_messages(
        &self,
        messages: &[LlmMessage],
    ) -> (Vec<LlmMessage>, Vec<LlmMessage>, Vec<LlmMessage>) {
        let len = messages.len();
        let protect = self.config.protect_recent_count.min(len);
        let pin_count = if self.config.pin_first_message && len > 0 {
            1
        } else {
            0
        };

        let pinned: Vec<LlmMessage> = messages[..pin_count].to_vec();
        let split_point = len.saturating_sub(protect);
        let compactable: Vec<LlmMessage> = messages[pin_count..split_point].to_vec();
        let protected: Vec<LlmMessage> = messages[split_point..].to_vec();

        (pinned, compactable, protected)
    }

    fn build_compaction_summary(&self, messages: &[LlmMessage]) -> String {
        let mut parts = Vec::new();
        for msg in messages {
            let role = &msg.role;
            let content = match &msg.content {
                LlmMessageContent::Text(t) => t.clone(),
                LlmMessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| match b {
                        LlmContentBlock::Text { text } => Some(text.as_str()),
                        LlmContentBlock::ToolUse { name, .. } => Some(name.as_str()),
                        LlmContentBlock::ToolResult { content, .. } => Some(content.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" | "),
            };
            if !content.trim().is_empty() {
                let truncated = if content.len() > 500 {
                    format!("{}...", &content[..500])
                } else {
                    content
                };
                parts.push(format!("[{}]: {}", role, truncated));
            }
        }
        parts.join("\n")
    }
}
