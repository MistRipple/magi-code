use std::collections::HashMap;

use magi_core::UtcMillis;

use crate::types::*;

pub struct NormalizerConfig {
    pub agent: String,
    pub default_source: MessageSource,
    pub debug: bool,
    pub caller_context: CallerContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallerContext {
    Orchestrator,
    Worker,
}

pub struct ParseContext {
    pub message_id: String,
    pub trace_id: String,
    pub metadata: serde_json::Value,
    pub blocks: Vec<ContentBlock>,
    pub pending_text: String,
    pub has_assistant_text: bool,
    pub pending_thinking: Option<String>,
    pub thinking_block_id: Option<String>,
    pub thinking_block_seq: u32,
    pub active_tool_calls: HashMap<String, ToolCallBlock>,
    pub interaction: Option<InteractionRequest>,
    pub duration_start_at: u64,
    pub visibility: Visibility,
}

pub enum NormalizerEvent {
    Message(StandardMessage),
    Update(StreamUpdate),
    Complete {
        message_id: String,
        message: StandardMessage,
    },
}

pub struct BaseNormalizer {
    pub config: NormalizerConfig,
    contexts: HashMap<String, ParseContext>,
    events: Vec<NormalizerEvent>,
}

impl BaseNormalizer {
    pub fn new(config: NormalizerConfig) -> Self {
        Self {
            config,
            contexts: HashMap::new(),
            events: Vec::new(),
        }
    }

    pub fn drain_events(&mut self) -> Vec<NormalizerEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn start_stream(
        &mut self,
        trace_id: &str,
        source: Option<MessageSource>,
        message_id_override: Option<&str>,
        visibility: Option<Visibility>,
    ) -> String {
        let message_id = message_id_override
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(generate_message_id);

        let now = UtcMillis::now().0;
        let vis = visibility.unwrap_or(Visibility::User);
        let context = ParseContext {
            message_id: message_id.clone(),
            trace_id: trace_id.to_string(),
            metadata: serde_json::json!({ "timelineAnchorTimestamp": now }),
            blocks: Vec::new(),
            pending_text: String::new(),
            has_assistant_text: false,
            pending_thinking: None,
            thinking_block_id: None,
            thinking_block_seq: 0,
            active_tool_calls: HashMap::new(),
            interaction: None,
            duration_start_at: now,
            visibility: vis,
        };

        self.contexts.insert(message_id.clone(), context);

        let vis_str = match vis {
            Visibility::User => "user",
            Visibility::System => "system",
            Visibility::Debug => "debug",
        };
        let message = StandardMessage {
            id: message_id.clone(),
            trace_id: trace_id.to_string(),
            category: MessageCategory::Content,
            message_type: MessageType::Text,
            source: source.unwrap_or(self.config.default_source),
            agent: self.config.agent.clone(),
            lifecycle: MessageLifecycle::Streaming,
            timestamp: now,
            updated_at: now,
            blocks: Vec::new(),
            interaction: None,
            visibility: vis_str.to_string(),
            metadata: serde_json::json!({ "timelineAnchorTimestamp": now }),
        };
        self.events.push(NormalizerEvent::Message(message));
        message_id
    }

    pub fn process_text_delta(&mut self, message_id: &str, delta: &str) {
        let Some(context) = self.contexts.get_mut(message_id) else {
            return;
        };
        if delta.is_empty() {
            return;
        }
        flush_pending_thinking_to_blocks(context);
        context.pending_text.push_str(delta);
        context.has_assistant_text = true;
        let update = StreamUpdate::AppendText {
            message_id: message_id.to_string(),
            timestamp: UtcMillis::now().0,
            text: delta.to_string(),
        };
        self.events.push(NormalizerEvent::Update(update));
    }

    pub fn process_thinking(&mut self, message_id: &str, thinking_content: &str) {
        let Some(context) = self.contexts.get_mut(message_id) else {
            return;
        };
        if context.pending_thinking.is_none() {
            context.pending_thinking = Some(String::new());
        }
        context
            .pending_thinking
            .as_mut()
            .unwrap()
            .push_str(thinking_content);
        if context.thinking_block_id.is_none() {
            context.thinking_block_id = Some(allocate_thinking_block_id(context));
        }
        let update = StreamUpdate::MergeBlock {
            message_id: message_id.to_string(),
            timestamp: UtcMillis::now().0,
            blocks: Some(vec![ContentBlock::Thinking(ThinkingBlock {
                content: context.pending_thinking.clone().unwrap_or_default(),
                summary: None,
                block_id: context.thinking_block_id.clone(),
            })]),
            token_usage: None,
        };
        self.events.push(NormalizerEvent::Update(update));
    }

    pub fn process_usage(&mut self, message_id: &str, usage: TokenUsageInfo) {
        if !self.contexts.contains_key(message_id) {
            return;
        }
        let update = StreamUpdate::MergeBlock {
            message_id: message_id.to_string(),
            timestamp: UtcMillis::now().0,
            blocks: None,
            token_usage: Some(usage),
        };
        self.events.push(NormalizerEvent::Update(update));
    }

    pub fn add_tool_call(&mut self, message_id: &str, tool_call: ToolCallBlock) {
        let Some(context) = self.contexts.get_mut(message_id) else {
            return;
        };
        flush_pending_thinking_to_blocks(context);
        flush_pending_text_to_blocks(context);
        let update = StreamUpdate::MergeBlock {
            message_id: message_id.to_string(),
            timestamp: UtcMillis::now().0,
            blocks: Some(vec![ContentBlock::ToolCall(tool_call.clone())]),
            token_usage: None,
        };
        context
            .active_tool_calls
            .insert(tool_call.tool_id.clone(), tool_call);
        self.events.push(NormalizerEvent::Update(update));
    }

    pub fn finish_tool_call(
        &mut self,
        message_id: &str,
        tool_id: &str,
        output: Option<String>,
        error: Option<String>,
    ) {
        let Some(context) = self.contexts.get_mut(message_id) else {
            return;
        };
        let tool_call = match context.active_tool_calls.get_mut(tool_id) {
            Some(tc) => tc,
            None => return,
        };
        if let Some(err) = &error {
            tool_call.status = ToolCallStatus::Failed;
            tool_call.error = Some(err.clone());
        } else {
            tool_call.status = ToolCallStatus::Completed;
            tool_call.output = output.clone();
        }
        let finished = tool_call.clone();
        context
            .blocks
            .push(ContentBlock::ToolCall(finished.clone()));
        context.active_tool_calls.remove(tool_id);

        let update = StreamUpdate::MergeBlock {
            message_id: message_id.to_string(),
            timestamp: UtcMillis::now().0,
            blocks: Some(vec![ContentBlock::ToolCall(finished)]),
            token_usage: None,
        };
        self.events.push(NormalizerEvent::Update(update));
    }

    pub fn end_stream(&mut self, message_id: &str, error: Option<&str>) -> Option<StandardMessage> {
        let mut context = self.contexts.remove(message_id)?;
        let message = build_final_message(&mut context, &self.config, error);
        self.events.push(NormalizerEvent::Complete {
            message_id: message_id.to_string(),
            message: message.clone(),
        });
        Some(message)
    }

    pub fn interrupt_stream(&mut self, message_id: &str) -> Option<StandardMessage> {
        let mut context = self.contexts.remove(message_id)?;
        let mut message = build_final_message(&mut context, &self.config, None);
        message.lifecycle = MessageLifecycle::Cancelled;
        self.events.push(NormalizerEvent::Complete {
            message_id: message_id.to_string(),
            message: message.clone(),
        });
        Some(message)
    }

    pub fn detach_stream(&mut self, message_id: &str) {
        self.contexts.remove(message_id);
    }

    pub fn has_active_stream(&self) -> bool {
        !self.contexts.is_empty()
    }

    pub fn active_message_ids(&self) -> Vec<String> {
        self.contexts.keys().cloned().collect()
    }

    pub fn get_context_mut(&mut self, message_id: &str) -> Option<&mut ParseContext> {
        self.contexts.get_mut(message_id)
    }

    pub fn push_event(&mut self, event: NormalizerEvent) {
        self.events.push(event);
    }
}

fn allocate_thinking_block_id(context: &mut ParseContext) -> String {
    context.thinking_block_seq += 1;
    format!(
        "{}-thinking-{}",
        context.message_id, context.thinking_block_seq
    )
}

pub fn flush_pending_thinking_to_blocks(context: &mut ParseContext) {
    let thinking = match context.pending_thinking.take() {
        Some(t) if !t.trim().is_empty() => t,
        _ => return,
    };
    context.blocks.push(ContentBlock::Thinking(ThinkingBlock {
        content: thinking.trim().to_string(),
        summary: None,
        block_id: context.thinking_block_id.take(),
    }));
}

pub fn flush_pending_text_to_blocks(context: &mut ParseContext) {
    let text = context.pending_text.trim().to_string();
    if text.is_empty() {
        return;
    }
    context.blocks.push(ContentBlock::Text(TextBlock {
        content: text,
        is_markdown: true,
    }));
    context.pending_text.clear();
}

fn build_final_message(
    context: &mut ParseContext,
    config: &NormalizerConfig,
    error: Option<&str>,
) -> StandardMessage {
    flush_pending_text_to_blocks(context);
    flush_pending_thinking_to_blocks(context);

    let mut blocks = context.blocks.clone();
    for tool_call in context.active_tool_calls.values() {
        blocks.push(ContentBlock::ToolCall(tool_call.clone()));
    }

    let message_type = if error.is_some() {
        MessageType::Error
    } else if context.interaction.is_some() {
        MessageType::Interaction
    } else if blocks.iter().any(|b| matches!(b, ContentBlock::Plan(_))) {
        MessageType::Plan
    } else if blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolCall(_)))
    {
        MessageType::ToolCall
    } else if blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::Thinking(_)))
    {
        MessageType::Thinking
    } else {
        MessageType::Text
    };

    let now = UtcMillis::now().0;
    let duration = now.saturating_sub(context.duration_start_at);
    let mut metadata = context.metadata.clone();
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("duration".to_string(), serde_json::json!(duration));
        if let Some(err) = error {
            obj.insert("error".to_string(), serde_json::json!(err));
        }
    }

    let vis_str = match context.visibility {
        Visibility::User => "user",
        Visibility::System => "system",
        Visibility::Debug => "debug",
    };

    StandardMessage {
        id: context.message_id.clone(),
        trace_id: context.trace_id.clone(),
        category: MessageCategory::Content,
        message_type,
        source: config.default_source,
        agent: config.agent.clone(),
        lifecycle: if error.is_some() {
            MessageLifecycle::Failed
        } else {
            MessageLifecycle::Completed
        },
        timestamp: context.duration_start_at,
        updated_at: now,
        blocks,
        interaction: context.interaction.take(),
        visibility: vis_str.to_string(),
        metadata,
    }
}
