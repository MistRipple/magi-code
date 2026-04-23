use crate::authority::{
    UsageAuthority, build_execution_binding_identity, build_usage_call_identity,
};
use crate::costing::normalize_usage_delta;
use crate::model_identity::{build_model_resolution_identity, canonicalize_base_url};
use crate::reducer::{
    rebuild_session_snapshot_from_events, rebuild_workspace_snapshot_from_sessions,
};
use crate::types::*;

fn make_llm_config() -> LlmConfig {
    LlmConfig {
        provider: "anthropic".to_string(),
        model: "claude-sonnet-4-20250514".to_string(),
        base_url: "https://api.anthropic.com/v1".to_string(),
        api_key: Some("sk-test-key-12345".to_string()),
        url_mode: UrlMode::Default,
        openai_protocol: None,
        reasoning_effort: None,
        enable_thinking: None,
    }
}

fn make_call_record_input(
    session_id: &str,
    call_id: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> UsageCallRecordInput {
    UsageCallRecordInput {
        workspace_id: "ws-1".to_string(),
        session_id: session_id.to_string(),
        turn_id: Some("turn-1".to_string()),
        dispatch_wave_id: None,
        assignment_id: Some("assign-1".to_string()),
        event_id: None,
        timestamp: Some(1700000000000),
        execution_binding: build_execution_binding_identity(
            "tmpl-1",
            "engine-1",
            1,
            UsageSourceRole::Worker,
        ),
        model_config: make_llm_config(),
        call_identity: build_usage_call_identity(
            call_id,
            None,
            UsageSourceRole::Worker,
            Some(UsagePhase::Execution),
        ),
        usage: UsageTokenInput {
            input_tokens,
            output_tokens,
            cache_read_tokens: Some(100),
            cache_write_tokens: Some(50),
        },
        status: UsageCallStatus::Success,
        error_code: None,
    }
}

#[test]
fn test_normalize_usage_delta() {
    let delta = UsageEventUsageDelta {
        raw_input_tokens: 1000,
        raw_output_tokens: 500,
        cache_read_tokens: Some(200),
        cache_write_tokens: Some(100),
    };
    let normalized = normalize_usage_delta(&delta);
    assert_eq!(normalized.raw_input_tokens, 1000);
    assert_eq!(normalized.raw_output_tokens, 500);
    assert_eq!(normalized.cache_read_tokens, 200);
    assert_eq!(normalized.cache_write_tokens, 100);
    assert_eq!(normalized.net_input_tokens, 800);
    assert_eq!(normalized.net_output_tokens, 500);
    assert_eq!(normalized.total_tokens, 1300);
}

#[test]
fn test_normalize_zero_cache() {
    let delta = UsageEventUsageDelta {
        raw_input_tokens: 500,
        raw_output_tokens: 300,
        cache_read_tokens: None,
        cache_write_tokens: None,
    };
    let normalized = normalize_usage_delta(&delta);
    assert_eq!(normalized.net_input_tokens, 500);
    assert_eq!(normalized.total_tokens, 800);
}

#[test]
fn test_canonicalize_base_url() {
    assert_eq!(
        canonicalize_base_url("https://API.Anthropic.com/v1/"),
        "https://api.anthropic.com/v1"
    );
    assert_eq!(
        canonicalize_base_url("  https://example.com:8080/api/ "),
        "https://example.com:8080/api"
    );
    assert_eq!(canonicalize_base_url(""), "");
    assert_eq!(canonicalize_base_url("not-a-url///"), "not-a-url");
}

#[test]
fn test_build_model_resolution_identity() {
    let config = make_llm_config();
    let binding =
        build_execution_binding_identity("tmpl-1", "engine-1", 1, UsageSourceRole::Worker);
    let identity =
        build_model_resolution_identity(&config, &binding, Some("claude-sonnet-4-20250514"), None);

    assert_eq!(identity.provider, "anthropic");
    assert_eq!(identity.resolved_model, "claude-sonnet-4-20250514");
    assert!(!identity.model_identity_key.is_empty());
    assert!(!identity.base_url_fingerprint.is_empty());
    assert!(identity.account_fingerprint.is_some());
    assert!(identity.openai_protocol.is_none());
}

#[test]
fn test_model_identity_key_deterministic() {
    let config = make_llm_config();
    let binding =
        build_execution_binding_identity("tmpl-1", "engine-1", 1, UsageSourceRole::Worker);
    let id1 = build_model_resolution_identity(&config, &binding, None, None);
    let id2 = build_model_resolution_identity(&config, &binding, None, None);
    assert_eq!(id1.model_identity_key, id2.model_identity_key);
}

#[test]
fn test_append_and_get_session_snapshot() {
    let mut authority = UsageAuthority::new();
    let input = make_call_record_input("sess-1", "call-1", 1000, 500);
    let seq = authority.append_call_record(input);
    assert_eq!(seq, 1);

    let snapshot = authority.get_session_snapshot("ws-1", "sess-1");
    assert_eq!(snapshot.version, 1);
    assert_eq!(snapshot.totals.llm_call_count, 1);
    assert_eq!(snapshot.totals.raw_input_tokens, 1000);
    assert_eq!(snapshot.totals.raw_output_tokens, 500);
    assert_eq!(snapshot.totals.success_count, 1);
    assert_eq!(snapshot.totals.failure_count, 0);
}

#[test]
fn test_multiple_calls_accumulate() {
    let mut authority = UsageAuthority::new();
    authority.append_call_record(make_call_record_input("sess-1", "call-1", 1000, 500));
    authority.append_call_record(make_call_record_input("sess-1", "call-2", 2000, 800));

    let snapshot = authority.get_session_snapshot("ws-1", "sess-1");
    assert_eq!(snapshot.totals.llm_call_count, 2);
    assert_eq!(snapshot.totals.raw_input_tokens, 3000);
    assert_eq!(snapshot.totals.raw_output_tokens, 1300);
}

#[test]
fn test_idempotent_append() {
    let mut authority = UsageAuthority::new();
    let mut input = make_call_record_input("sess-1", "call-1", 1000, 500);
    input.event_id = Some("fixed-event-id".to_string());
    authority.append_call_record(input.clone());
    authority.append_call_record(input);

    let snapshot = authority.get_session_snapshot("ws-1", "sess-1");
    assert_eq!(snapshot.totals.llm_call_count, 1);
}

#[test]
fn test_workspace_snapshot_aggregation() {
    let mut authority = UsageAuthority::new();
    authority.append_call_record(make_call_record_input("sess-1", "call-1", 1000, 500));
    authority.append_call_record(make_call_record_input("sess-2", "call-2", 2000, 800));

    let snapshot = authority.get_workspace_snapshot("ws-1");
    assert_eq!(snapshot.totals.raw_input_tokens, 3000);
    assert_eq!(snapshot.by_session.len(), 2);
}

#[test]
fn test_session_reset() {
    let mut authority = UsageAuthority::new();
    authority.append_call_record(make_call_record_input("sess-1", "call-1", 1000, 500));

    let before = authority.get_session_snapshot("ws-1", "sess-1");
    assert_eq!(before.totals.llm_call_count, 1);

    authority.reset_session("ws-1", "sess-1");

    let after = authority.get_session_snapshot("ws-1", "sess-1");
    assert_eq!(after.totals.llm_call_count, 0);
    assert!(after.version > before.version);
}

#[test]
fn test_failed_call_status() {
    let mut authority = UsageAuthority::new();
    let mut input = make_call_record_input("sess-1", "call-1", 500, 200);
    input.status = UsageCallStatus::Failed;
    input.error_code = Some("rate_limit".to_string());
    authority.append_call_record(input);

    let snapshot = authority.get_session_snapshot("ws-1", "sess-1");
    assert_eq!(snapshot.totals.success_count, 0);
    assert_eq!(snapshot.totals.failure_count, 1);
}

#[test]
fn test_binding_and_model_snapshots() {
    let mut authority = UsageAuthority::new();
    authority.append_call_record(make_call_record_input("sess-1", "call-1", 1000, 500));

    let snapshot = authority.get_session_snapshot("ws-1", "sess-1");
    assert_eq!(snapshot.by_execution_binding.len(), 1);
    assert_eq!(snapshot.by_model_identity.len(), 1);

    let binding = &snapshot.by_execution_binding[0];
    assert_eq!(binding.template_id, "tmpl-1");
    assert_eq!(binding.totals.llm_call_count, 1);

    let model = &snapshot.by_model_identity[0];
    assert_eq!(model.provider, "anthropic");
    assert_eq!(model.totals.llm_call_count, 1);
}

#[test]
fn test_rebuild_session_from_events() {
    let binding =
        build_execution_binding_identity("tmpl-1", "engine-1", 1, UsageSourceRole::Worker);
    let config = make_llm_config();
    let model_id = build_model_resolution_identity(&config, &binding, None, None);

    let events = vec![
        UsageEvent {
            event_id: "e1".to_string(),
            ledger_seq: 1,
            workspace_id: "ws-1".to_string(),
            session_id: "sess-1".to_string(),
            turn_id: Some("t1".to_string()),
            dispatch_wave_id: None,
            assignment_id: Some("a1".to_string()),
            timestamp: 1000,
            event_type: UsageEventType::LlmCallCompleted,
            execution_binding: Some(binding.clone()),
            model_identity: Some(model_id.clone()),
            call_identity: Some(build_usage_call_identity(
                "c1",
                None,
                UsageSourceRole::Worker,
                None,
            )),
            usage_delta: Some(UsageEventUsageDelta {
                raw_input_tokens: 500,
                raw_output_tokens: 200,
                cache_read_tokens: None,
                cache_write_tokens: None,
            }),
            status: Some(UsageCallStatus::Success),
            error_code: None,
        },
        UsageEvent {
            event_id: "e2".to_string(),
            ledger_seq: 2,
            workspace_id: "ws-1".to_string(),
            session_id: "sess-1".to_string(),
            turn_id: Some("t1".to_string()),
            dispatch_wave_id: None,
            assignment_id: Some("a1".to_string()),
            timestamp: 2000,
            event_type: UsageEventType::LlmCallCompleted,
            execution_binding: Some(binding),
            model_identity: Some(model_id),
            call_identity: Some(build_usage_call_identity(
                "c2",
                None,
                UsageSourceRole::Worker,
                None,
            )),
            usage_delta: Some(UsageEventUsageDelta {
                raw_input_tokens: 300,
                raw_output_tokens: 100,
                cache_read_tokens: None,
                cache_write_tokens: None,
            }),
            status: Some(UsageCallStatus::Success),
            error_code: None,
        },
    ];

    let snapshot = rebuild_session_snapshot_from_events("ws-1", "sess-1", &events);
    assert_eq!(snapshot.version, 2);
    assert_eq!(snapshot.totals.llm_call_count, 2);
    assert_eq!(snapshot.totals.raw_input_tokens, 800);
    assert_eq!(snapshot.totals.assignment_count, 1);
    assert_eq!(snapshot.totals.turn_count, 1);
}

#[test]
fn test_rebuild_workspace_from_sessions() {
    let s1 = SessionUsageSnapshot {
        workspace_id: "ws-1".to_string(),
        session_id: "s1".to_string(),
        version: 2,
        last_applied_ledger_seq: 2,
        updated_at: 2000,
        totals: UsageTotals {
            llm_call_count: 2,
            raw_input_tokens: 1000,
            raw_output_tokens: 500,
            total_tokens: 1500,
            success_count: 2,
            ..Default::default()
        },
        by_execution_binding: vec![],
        by_model_identity: vec![],
    };
    let s2 = SessionUsageSnapshot {
        workspace_id: "ws-1".to_string(),
        session_id: "s2".to_string(),
        version: 1,
        last_applied_ledger_seq: 1,
        updated_at: 3000,
        totals: UsageTotals {
            llm_call_count: 1,
            raw_input_tokens: 500,
            raw_output_tokens: 200,
            total_tokens: 700,
            success_count: 1,
            ..Default::default()
        },
        by_execution_binding: vec![],
        by_model_identity: vec![],
    };

    let ws = rebuild_workspace_snapshot_from_sessions("ws-1", &[s1, s2]);
    assert_eq!(ws.totals.llm_call_count, 3);
    assert_eq!(ws.totals.raw_input_tokens, 1500);
    assert_eq!(ws.by_session.len(), 2);
    assert_eq!(ws.version, 2);
    assert_eq!(ws.updated_at, 3000);
}

#[test]
fn test_usage_totals_add() {
    let a = UsageTotals {
        llm_call_count: 5,
        raw_input_tokens: 1000,
        raw_output_tokens: 500,
        success_count: 4,
        failure_count: 1,
        ..Default::default()
    };
    let b = UsageTotals {
        llm_call_count: 3,
        raw_input_tokens: 600,
        raw_output_tokens: 300,
        success_count: 3,
        ..Default::default()
    };
    let sum = a.add(&b);
    assert_eq!(sum.llm_call_count, 8);
    assert_eq!(sum.raw_input_tokens, 1600);
    assert_eq!(sum.success_count, 7);
    assert_eq!(sum.failure_count, 1);
}

#[test]
fn test_session_reset_rebuilds_from_events() {
    let binding =
        build_execution_binding_identity("tmpl-1", "engine-1", 1, UsageSourceRole::Worker);
    let config = make_llm_config();
    let model_id = build_model_resolution_identity(&config, &binding, None, None);

    let events = vec![
        UsageEvent {
            event_id: "e1".to_string(),
            ledger_seq: 1,
            workspace_id: "ws-1".to_string(),
            session_id: "sess-1".to_string(),
            turn_id: None,
            dispatch_wave_id: None,
            assignment_id: None,
            timestamp: 1000,
            event_type: UsageEventType::LlmCallCompleted,
            execution_binding: Some(binding.clone()),
            model_identity: Some(model_id.clone()),
            call_identity: Some(build_usage_call_identity(
                "c1",
                None,
                UsageSourceRole::Worker,
                None,
            )),
            usage_delta: Some(UsageEventUsageDelta {
                raw_input_tokens: 500,
                raw_output_tokens: 200,
                cache_read_tokens: None,
                cache_write_tokens: None,
            }),
            status: Some(UsageCallStatus::Success),
            error_code: None,
        },
        UsageEvent {
            event_id: "reset-1".to_string(),
            ledger_seq: 2,
            workspace_id: "ws-1".to_string(),
            session_id: "sess-1".to_string(),
            turn_id: None,
            dispatch_wave_id: None,
            assignment_id: None,
            timestamp: 2000,
            event_type: UsageEventType::SessionReset,
            execution_binding: None,
            model_identity: None,
            call_identity: None,
            usage_delta: None,
            status: None,
            error_code: None,
        },
        UsageEvent {
            event_id: "e2".to_string(),
            ledger_seq: 3,
            workspace_id: "ws-1".to_string(),
            session_id: "sess-1".to_string(),
            turn_id: None,
            dispatch_wave_id: None,
            assignment_id: None,
            timestamp: 3000,
            event_type: UsageEventType::LlmCallCompleted,
            execution_binding: Some(binding),
            model_identity: Some(model_id),
            call_identity: Some(build_usage_call_identity(
                "c2",
                None,
                UsageSourceRole::Worker,
                None,
            )),
            usage_delta: Some(UsageEventUsageDelta {
                raw_input_tokens: 300,
                raw_output_tokens: 100,
                cache_read_tokens: None,
                cache_write_tokens: None,
            }),
            status: Some(UsageCallStatus::Success),
            error_code: None,
        },
    ];

    let snapshot = rebuild_session_snapshot_from_events("ws-1", "sess-1", &events);
    assert_eq!(snapshot.totals.llm_call_count, 1);
    assert_eq!(snapshot.totals.raw_input_tokens, 300);
    assert_eq!(snapshot.version, 3);
}

#[test]
fn test_empty_session_snapshot() {
    let snapshot = SessionUsageSnapshot::empty("ws-1", "sess-1");
    assert_eq!(snapshot.version, 0);
    assert_eq!(snapshot.totals.llm_call_count, 0);
    assert!(snapshot.by_execution_binding.is_empty());
}
