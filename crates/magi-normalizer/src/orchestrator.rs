use magi_core::UtcMillis;

use crate::types::*;

pub fn normalize_orchestrator_message(
    content: &str,
    message_type_hint: &str,
    trace_id: Option<&str>,
) -> StandardMessage {
    let message_id = generate_message_id();
    let now = UtcMillis::now().0;

    let message_type = match message_type_hint {
        "error" => MessageType::Error,
        "progress_update" => MessageType::Progress,
        "plan_ready" => MessageType::Plan,
        "summary" => MessageType::Result,
        _ => MessageType::Text,
    };

    let lifecycle = match message_type_hint {
        "progress_update" => MessageLifecycle::Streaming,
        "error" => MessageLifecycle::Failed,
        _ => MessageLifecycle::Completed,
    };

    let blocks = if content.is_empty() {
        Vec::new()
    } else {
        vec![ContentBlock::Text(TextBlock {
            content: content.to_string(),
            is_markdown: message_type_hint == "plan_ready" || message_type_hint == "summary",
        })]
    };

    StandardMessage {
        id: message_id,
        trace_id: trace_id
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("trace-{}", &uuid::Uuid::new_v4().to_string()[..8])),
        category: MessageCategory::Content,
        message_type,
        source: MessageSource::Orchestrator,
        agent: "orchestrator".to_string(),
        lifecycle,
        timestamp: now,
        updated_at: now,
        blocks,
        interaction: None,
        visibility: "user".to_string(),
        metadata: serde_json::Value::Object(serde_json::Map::new()),
    }
}

pub fn create_orchestrator_update(message_id: &str, content: &str) -> StreamUpdate {
    StreamUpdate::AppendText {
        message_id: message_id.to_string(),
        timestamp: UtcMillis::now().0,
        text: content.to_string(),
    }
}

pub fn is_internal_state_message(content: &str) -> bool {
    let patterns = [
        "正在分析任务依赖关系",
        "执行模式已调整",
        "已移除冗余",
        "编排纪律提示",
    ];
    patterns.iter().any(|p| content.starts_with(p))
}

pub fn get_message_priority(message_type: &str) -> u32 {
    match message_type {
        "error" => 100,
        "summary" => 90,
        "plan_ready" => 80,
        "direct_response" => 70,
        "progress_update" => 50,
        _ => 0,
    }
}
