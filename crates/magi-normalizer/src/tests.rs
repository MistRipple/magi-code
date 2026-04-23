use crate::base::*;
use crate::claude::*;
use crate::codex::*;
use crate::gemini::*;
use crate::orchestrator::*;
use crate::types::*;

#[test]
fn test_base_normalizer_stream_lifecycle() {
    let mut n = BaseNormalizer::new(NormalizerConfig {
        agent: "test".to_string(),
        default_source: MessageSource::Worker,
        debug: false,
        caller_context: CallerContext::Worker,
    });
    let mid = n.start_stream("trace-1", None, None, None);
    assert!(!mid.is_empty());
    assert!(n.has_active_stream());

    n.process_text_delta(&mid, "hello ");
    n.process_text_delta(&mid, "world");

    let msg = n.end_stream(&mid, None).unwrap();
    assert_eq!(msg.lifecycle, MessageLifecycle::Completed);
    assert!(
        msg.blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text(t) if t.content == "hello world"))
    );
    assert!(!n.has_active_stream());
}

#[test]
fn test_thinking_blocks() {
    let mut n = BaseNormalizer::new(NormalizerConfig {
        agent: "test".to_string(),
        default_source: MessageSource::Worker,
        debug: false,
        caller_context: CallerContext::Worker,
    });
    let mid = n.start_stream("trace-1", None, None, None);
    n.process_thinking(&mid, "let me think...");
    n.process_text_delta(&mid, "answer");

    let msg = n.end_stream(&mid, None).unwrap();
    assert!(
        msg.blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Thinking(_)))
    );
    assert!(
        msg.blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text(_)))
    );
}

#[test]
fn test_tool_call_lifecycle() {
    let mut n = BaseNormalizer::new(NormalizerConfig {
        agent: "test".to_string(),
        default_source: MessageSource::Worker,
        debug: false,
        caller_context: CallerContext::Worker,
    });
    let mid = n.start_stream("trace-1", None, None, None);
    n.add_tool_call(
        &mid,
        ToolCallBlock {
            tool_name: "read_file".to_string(),
            tool_id: "tool-1".to_string(),
            status: ToolCallStatus::Running,
            input: Some(r#"{"path": "foo.rs"}"#.to_string()),
            output: None,
            error: None,
        },
    );
    n.finish_tool_call(&mid, "tool-1", Some("file content".to_string()), None);

    let msg = n.end_stream(&mid, None).unwrap();
    assert_eq!(msg.message_type, MessageType::ToolCall);
    let tool_blocks: Vec<_> = msg
        .blocks
        .iter()
        .filter(|b| matches!(b, ContentBlock::ToolCall(_)))
        .collect();
    assert_eq!(tool_blocks.len(), 1);
}

#[test]
fn test_error_stream() {
    let mut n = BaseNormalizer::new(NormalizerConfig {
        agent: "test".to_string(),
        default_source: MessageSource::Worker,
        debug: false,
        caller_context: CallerContext::Worker,
    });
    let mid = n.start_stream("trace-1", None, None, None);
    n.process_text_delta(&mid, "partial output");

    let msg = n.end_stream(&mid, Some("connection lost")).unwrap();
    assert_eq!(msg.message_type, MessageType::Error);
    assert_eq!(msg.lifecycle, MessageLifecycle::Failed);
}

#[test]
fn test_interrupt_stream() {
    let mut n = BaseNormalizer::new(NormalizerConfig {
        agent: "test".to_string(),
        default_source: MessageSource::Worker,
        debug: false,
        caller_context: CallerContext::Worker,
    });
    let mid = n.start_stream("trace-1", None, None, None);
    n.process_text_delta(&mid, "partial");

    let msg = n.interrupt_stream(&mid).unwrap();
    assert_eq!(msg.lifecycle, MessageLifecycle::Cancelled);
    assert!(!n.has_active_stream());
}

#[test]
fn test_claude_normalizer_text_delta() {
    let mut cn = create_claude_normalizer(
        "claude",
        MessageSource::Worker,
        false,
        CallerContext::Worker,
    );
    let mid = cn.normalizer.start_stream("trace-1", None, None, None);
    cn.parse_chunk(
        &mid,
        r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}"#,
    );
    cn.parse_chunk(&mid, "\n");

    let msg = cn.normalizer.end_stream(&mid, None).unwrap();
    assert!(
        msg.blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text(t) if t.content.contains("hello")))
    );
}

#[test]
fn test_claude_normalizer_thinking() {
    let mut cn = create_claude_normalizer(
        "claude",
        MessageSource::Worker,
        false,
        CallerContext::Worker,
    );
    let mid = cn.normalizer.start_stream("trace-1", None, None, None);
    cn.parse_chunk(&mid, r#"{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"reasoning..."}}"#);
    cn.parse_chunk(&mid, "\n");
    cn.parse_chunk(
        &mid,
        r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"answer"}}"#,
    );
    cn.parse_chunk(&mid, "\n");

    let msg = cn.normalizer.end_stream(&mid, None).unwrap();
    assert!(
        msg.blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Thinking(t) if t.content.contains("reasoning")))
    );
    assert!(
        msg.blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text(t) if t.content.contains("answer")))
    );
}

#[test]
fn test_codex_normalizer_json() {
    let mut cn =
        create_codex_normalizer("codex", MessageSource::Worker, false, CallerContext::Worker);
    let mid = cn.normalizer.start_stream("trace-1", None, None, None);
    cn.parse_chunk(&mid, r#"{"type":"message","text":"hello from codex"}"#);
    cn.parse_chunk(&mid, "\n");

    let msg = cn.normalizer.end_stream(&mid, None).unwrap();
    assert!(
        msg.blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text(t) if t.content.contains("hello from codex")))
    );
}

#[test]
fn test_codex_normalizer_plain_text() {
    let mut cn =
        create_codex_normalizer("codex", MessageSource::Worker, false, CallerContext::Worker);
    let mid = cn.normalizer.start_stream("trace-1", None, None, None);
    cn.parse_chunk(&mid, "just plain text\n");

    let msg = cn.normalizer.end_stream(&mid, None).unwrap();
    assert!(
        msg.blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text(t) if t.content.contains("plain text")))
    );
}

#[test]
fn test_gemini_normalizer_json() {
    let mut gn = create_gemini_normalizer(
        "gemini",
        MessageSource::Worker,
        false,
        CallerContext::Worker,
    );
    let mid = gn.normalizer.start_stream("trace-1", None, None, None);
    gn.parse_chunk(&mid, r#"{"type":"text","content":"gemini response"}"#);

    let msg = gn.normalizer.end_stream(&mid, None).unwrap();
    assert!(
        msg.blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text(t) if t.content.contains("gemini response")))
    );
}

#[test]
fn test_gemini_normalizer_thinking() {
    let mut gn = create_gemini_normalizer(
        "gemini",
        MessageSource::Worker,
        false,
        CallerContext::Worker,
    );
    let mid = gn.normalizer.start_stream("trace-1", None, None, None);
    gn.parse_chunk(&mid, r#"{"type":"thinking","content":"deep thought"}"#);

    let msg = gn.normalizer.end_stream(&mid, None).unwrap();
    assert!(
        msg.blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Thinking(t) if t.content.contains("deep thought")))
    );
}

#[test]
fn test_orchestrator_normalizer() {
    let msg = normalize_orchestrator_message("任务已完成", "summary", Some("trace-1"));
    assert_eq!(msg.message_type, MessageType::Result);
    assert_eq!(msg.lifecycle, MessageLifecycle::Completed);
    assert_eq!(msg.source, MessageSource::Orchestrator);
}

#[test]
fn test_orchestrator_error() {
    let msg = normalize_orchestrator_message("执行失败", "error", None);
    assert_eq!(msg.message_type, MessageType::Error);
    assert_eq!(msg.lifecycle, MessageLifecycle::Failed);
}

#[test]
fn test_internal_state_detection() {
    assert!(is_internal_state_message("正在分析任务依赖关系..."));
    assert!(is_internal_state_message("执行模式已调整为 parallel"));
    assert!(!is_internal_state_message("任务已完成"));
}

#[test]
fn test_message_priority() {
    assert!(get_message_priority("error") > get_message_priority("summary"));
    assert!(get_message_priority("summary") > get_message_priority("progress_update"));
    assert_eq!(get_message_priority("unknown"), 0);
}

#[test]
fn test_generate_message_id() {
    let id1 = generate_message_id();
    let id2 = generate_message_id();
    assert_ne!(id1, id2);
    assert!(id1.starts_with("msg-"));
}

#[test]
fn test_drain_events() {
    let mut n = BaseNormalizer::new(NormalizerConfig {
        agent: "test".to_string(),
        default_source: MessageSource::Worker,
        debug: false,
        caller_context: CallerContext::Worker,
    });
    let mid = n.start_stream("trace-1", None, None, None);
    n.process_text_delta(&mid, "test");
    n.end_stream(&mid, None);

    let events = n.drain_events();
    assert!(events.len() >= 3);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, NormalizerEvent::Message(_)))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, NormalizerEvent::Update(_)))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, NormalizerEvent::Complete { .. }))
    );

    let events2 = n.drain_events();
    assert!(events2.is_empty());
}

#[test]
fn test_usage_reporting() {
    let mut n = BaseNormalizer::new(NormalizerConfig {
        agent: "test".to_string(),
        default_source: MessageSource::Worker,
        debug: false,
        caller_context: CallerContext::Worker,
    });
    let mid = n.start_stream("trace-1", None, None, None);
    n.process_usage(
        &mid,
        TokenUsageInfo {
            input_tokens: Some(100),
            output_tokens: Some(50),
            cache_read_tokens: None,
            cache_write_tokens: None,
        },
    );
    n.end_stream(&mid, None);

    let events = n.drain_events();
    assert!(events.iter().any(|e| matches!(
        e,
        NormalizerEvent::Update(StreamUpdate::MergeBlock {
            token_usage: Some(_),
            ..
        })
    )));
}
