use crate::authority::{
    UsageAuthority, build_execution_binding_identity, build_usage_call_identity,
};
use crate::costing::{
    context_window_tokens_from_usage, normalize_usage_delta, provider_context_tokens_from_usage,
};
use crate::model_identity::{build_model_resolution_identity, canonicalize_base_url};
use crate::reducer::{rebuild_usage_snapshot_from_events, rebuild_workspace_snapshot};
use crate::types::*;

fn make_llm_config() -> LlmConfig {
    LlmConfig {
        provider: "anthropic".to_string(),
        model: "claude-sonnet-4-20250514".to_string(),
        base_url: "https://api.anthropic.com/v1".to_string(),
        api_key: Some("sk-test-key-12345".to_string()),
        account_fingerprint: None,
        reasoning_effort: None,
    }
}

#[test]
fn cache_observation_excludes_missing_usage_and_normalizes_provider_denominators() {
    let mut authority = UsageAuthority::new();
    for (index, (read, included)) in [
        (Some(400), true),
        (Some(0), true),
        (None, true),
        (Some(400), false),
    ]
    .into_iter()
    .enumerate()
    {
        let mut input = make_call_record_input("session-cache", &format!("call-{index}"), 1000, 10);
        input.usage.cache_read_tokens = read;
        input.usage.cache_read_included_in_input = included;
        input.usage.cache_write_tokens = (!included).then_some(200);
        let copy = input.clone();
        authority.append_call_record(input);
        authority.append_call_record(copy);
    }
    let snapshot = authority.get_session_snapshot("ws-1", "session-cache");
    assert_eq!(snapshot.totals.llm_call_count, 4);
    assert_eq!(snapshot.totals.cache_observed_call_count, 3);
    assert_eq!(snapshot.totals.cache_hit_call_count, 2);
    assert_eq!(snapshot.totals.cache_observed_input_tokens, 3600);
    assert_eq!(snapshot.totals.cache_read_tokens, 800);
    let workspace = authority.get_workspace_snapshot("ws-1");
    for totals in [
        &workspace.totals,
        &snapshot.by_execution_binding[0].totals,
        &snapshot.by_model_identity[0].totals,
    ] {
        assert_eq!(totals.cache_observed_call_count, 3);
        assert_eq!(totals.cache_observed_input_tokens, 3600);
    }
    let mut old = serde_json::to_value(&snapshot.totals).unwrap();
    for key in [
        "cacheObservedCallCount",
        "cacheHitCallCount",
        "cacheObservedInputTokens",
    ] {
        old.as_object_mut().unwrap().remove(key);
    }
    let old: UsageTotals = serde_json::from_value(old).unwrap();
    assert_eq!(old.cache_observed_call_count, 0);
}

fn make_call_record_input(
    session_id: &str,
    call_id: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> UsageCallRecordInput {
    UsageCallRecordInput {
        workspace_id: Some("ws-1".to_string()),
        owner: UsageOwner::session(session_id),
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
            total_tokens: None,
            cache_read_tokens: Some(100),
            cache_write_tokens: Some(50),
            cache_read_included_in_input: false,
        },
        status: UsageCallStatus::Success,
        error_code: None,
    }
}

#[test]
fn plugin_and_session_ownership_are_disjoint_and_deduplicated() {
    let mut authority = UsageAuthority::new();
    let input = make_call_record_input("session-1", "call-1", 10, 2);
    authority.append_call_record(input.clone());
    let mut plugin = input;
    plugin.turn_id = None;
    plugin.assignment_id = None;
    plugin.owner = UsageOwner::Plugin {
        plugin: PluginModelOwner {
            plugin_id: "example".into(),
            digest: "a".repeat(64),
            instance_id: "instance".into(),
            operation_id: "operation".into(),
            display_name: Some("Example".into()),
        },
    };
    let mut both = serde_json::to_value(&plugin).unwrap();
    both["sessionId"] = serde_json::json!("session-1");
    assert!(serde_json::from_value::<UsageCallRecordInput>(both).is_err());
    authority.append_call_record(plugin.clone());
    authority.append_call_record(plugin.clone());
    let all = authority.get_workspace_snapshot("ws-1");
    assert_eq!(all.totals.llm_call_count, 2);
    assert_eq!(all.by_session.len(), 1);
    assert_eq!(all.by_execution_binding.len(), 2);
    assert_eq!(all.by_model_identity.len(), 1);
    assert_eq!(
        authority
            .get_session_snapshot("ws-1", "session-1")
            .totals
            .llm_call_count,
        1
    );
    plugin.call_identity.call_id = "call-2".into();
    authority.append_call_record(plugin);
    assert_eq!(
        authority
            .get_workspace_snapshot("ws-1")
            .totals
            .llm_call_count,
        3
    );
}

#[test]
fn persisted_llm_config_redacts_secret_and_preserves_account_identity() {
    let config = make_llm_config();
    let binding = build_execution_binding_identity(
        "orchestrator",
        "orchestrator",
        0,
        UsageSourceRole::Orchestrator,
    );
    let original_identity =
        build_model_resolution_identity(&config, &binding, Some(&config.model), None);

    let persisted = crate::prepare_llm_config_for_persistence(config);
    let replayed_identity =
        build_model_resolution_identity(&persisted, &binding, Some(&persisted.model), None);

    assert!(persisted.api_key.is_none());
    assert!(persisted.account_fingerprint.is_some());
    assert_eq!(
        original_identity.account_fingerprint,
        replayed_identity.account_fingerprint
    );
    assert_eq!(
        original_identity.model_identity_key,
        replayed_identity.model_identity_key
    );
}

#[test]
fn test_normalize_usage_delta() {
    let delta = UsageEventUsageDelta {
        raw_input_tokens: 1000,
        raw_output_tokens: 500,
        context_window_tokens: None,
        cache_read_tokens: Some(200),
        cache_write_tokens: Some(100),
        cache_read_included_in_input: false,
    };
    let normalized = normalize_usage_delta(&delta);
    assert_eq!(normalized.raw_input_tokens, 1000);
    assert_eq!(normalized.raw_output_tokens, 500);
    assert_eq!(normalized.cache_read_tokens, 200);
    assert_eq!(normalized.cache_write_tokens, 100);
    assert_eq!(normalized.net_input_tokens, 1000);
    assert_eq!(normalized.net_output_tokens, 500);
    assert_eq!(normalized.total_tokens, 1500);
}

#[test]
fn context_window_tokens_do_not_double_count_openai_cache_reads() {
    let usage = UsageTokenInput {
        input_tokens: 1_000,
        output_tokens: 200,
        total_tokens: None,
        cache_read_tokens: Some(700),
        cache_write_tokens: None,
        cache_read_included_in_input: true,
    };

    assert_eq!(context_window_tokens_from_usage(&usage), 1_200);
}

#[test]
fn context_window_tokens_include_anthropic_cache_reads_and_writes() {
    let usage = UsageTokenInput {
        input_tokens: 300,
        output_tokens: 200,
        total_tokens: None,
        cache_read_tokens: Some(1_000),
        cache_write_tokens: Some(500),
        cache_read_included_in_input: false,
    };

    assert_eq!(context_window_tokens_from_usage(&usage), 2_000);
}

#[test]
fn context_window_tokens_prefer_provider_authoritative_total() {
    let usage = UsageTokenInput {
        input_tokens: 300,
        output_tokens: 200,
        total_tokens: Some(750),
        cache_read_tokens: Some(1_000),
        cache_write_tokens: Some(500),
        cache_read_included_in_input: false,
    };

    assert_eq!(context_window_tokens_from_usage(&usage), 750);
}

#[test]
fn provider_context_tokens_do_not_include_completion_tokens() {
    let usage = UsageTokenInput {
        input_tokens: 10_000,
        output_tokens: 2_000,
        total_tokens: Some(12_000),
        cache_read_tokens: None,
        cache_write_tokens: None,
        cache_read_included_in_input: false,
    };
    assert_eq!(provider_context_tokens_from_usage(&usage), 10_000);
}

#[test]
fn test_normalize_usage_delta_subtracts_included_cache_read() {
    let delta = UsageEventUsageDelta {
        raw_input_tokens: 1000,
        raw_output_tokens: 500,
        context_window_tokens: None,
        cache_read_tokens: Some(200),
        cache_write_tokens: Some(100),
        cache_read_included_in_input: true,
    };
    let normalized = normalize_usage_delta(&delta);
    assert_eq!(normalized.net_input_tokens, 800);
    assert_eq!(normalized.total_tokens, 1300);
}

#[test]
fn test_normalize_zero_cache() {
    let delta = UsageEventUsageDelta {
        raw_input_tokens: 500,
        raw_output_tokens: 300,
        context_window_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        cache_read_included_in_input: false,
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
fn test_model_identity_is_independent_from_role_binding_revision() {
    let config = make_llm_config();
    let first_binding =
        build_execution_binding_identity("tmpl-1", "engine-1", 1, UsageSourceRole::Worker);
    let second_binding =
        build_execution_binding_identity("tmpl-1", "engine-2", 2, UsageSourceRole::Worker);
    let first = build_model_resolution_identity(&config, &first_binding, None, None);
    let second = build_model_resolution_identity(&config, &second_binding, None, None);

    assert_eq!(first.model_identity_key, second.model_identity_key);
    assert_ne!(first.binding_revision, second.binding_revision);
}

#[test]
fn test_model_identity_keeps_reasoning_effort_separate() {
    let mut medium_config = make_llm_config();
    medium_config.reasoning_effort = Some(ReasoningEffort::Medium);
    let mut high_config = medium_config.clone();
    high_config.reasoning_effort = Some(ReasoningEffort::High);
    let binding =
        build_execution_binding_identity("tmpl-1", "engine-1", 1, UsageSourceRole::Worker);

    let medium = build_model_resolution_identity(&medium_config, &binding, None, None);
    let high = build_model_resolution_identity(&high_config, &binding, None, None);

    assert_ne!(medium.model_identity_key, high.model_identity_key);
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
    assert_eq!(snapshot.totals.cache_read_tokens, 100);
    assert_eq!(snapshot.totals.cache_write_tokens, 50);
    assert_eq!(snapshot.totals.net_input_tokens, 1000);
    assert_eq!(snapshot.totals.net_output_tokens, 500);
    assert_eq!(snapshot.totals.total_tokens, 1500);
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
fn test_switching_model_preserves_each_model_usage_snapshot() {
    let mut authority = UsageAuthority::new();
    authority.append_call_record(make_call_record_input("sess-1", "call-1", 1000, 500));

    let mut second = make_call_record_input("sess-1", "call-2", 600, 300);
    second.model_config.model = "gpt-5.6-luna".to_string();
    second.model_config.provider = "openai".to_string();
    second.model_config.base_url = "https://api.openai.com/v1".to_string();
    authority.append_call_record(second);

    let snapshot = authority.get_session_snapshot("ws-1", "sess-1");
    assert_eq!(snapshot.by_execution_binding.len(), 2);
    assert_eq!(
        snapshot
            .by_execution_binding
            .iter()
            .map(|binding| binding.totals.llm_call_count)
            .sum::<u64>(),
        2
    );
    assert_eq!(snapshot.by_model_identity.len(), 2);

    let mut models = snapshot
        .by_model_identity
        .iter()
        .map(|model| model.resolved_model.as_str())
        .collect::<Vec<_>>();
    models.sort_unstable();
    assert_eq!(models, vec!["claude-sonnet-4-20250514", "gpt-5.6-luna"]);
    assert_eq!(
        snapshot
            .by_model_identity
            .iter()
            .map(|model| model.totals.llm_call_count)
            .sum::<u64>(),
        2
    );
    assert_eq!(snapshot.totals.total_tokens, 2400);
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
            workspace_id: Some("ws-1".to_string()),
            owner: UsageOwner::session("sess-1"),
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
                context_window_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cache_read_included_in_input: false,
            }),
            status: Some(UsageCallStatus::Success),
            error_code: None,
        },
        UsageEvent {
            event_id: "e2".to_string(),
            ledger_seq: 2,
            workspace_id: Some("ws-1".to_string()),
            owner: UsageOwner::session("sess-1"),
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
                context_window_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cache_read_included_in_input: false,
            }),
            status: Some(UsageCallStatus::Success),
            error_code: None,
        },
    ];

    let snapshot = rebuild_usage_snapshot_from_events(&events).into_session(Some("ws-1"), "sess-1");
    assert_eq!(snapshot.version, 2);
    assert_eq!(snapshot.totals.llm_call_count, 2);
    assert_eq!(snapshot.totals.raw_input_tokens, 800);
    assert_eq!(snapshot.totals.assignment_count, 1);
    assert_eq!(snapshot.totals.turn_count, 1);
}

#[test]
fn test_rebuild_workspace_from_sessions() {
    let s1 = SessionUsageSnapshot {
        workspace_id: Some("ws-1".to_string()),
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
        workspace_id: Some("ws-1".to_string()),
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

    let ws = rebuild_workspace_snapshot(
        "ws-1",
        &[
            (
                UsageSubject::Session(s1.session_id.clone()),
                UsageAggregateSnapshot::from(&s1),
            ),
            (
                UsageSubject::Session(s2.session_id.clone()),
                UsageAggregateSnapshot::from(&s2),
            ),
        ],
    );
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
fn test_empty_session_snapshot() {
    let snapshot = SessionUsageSnapshot::empty(Some("ws-1"), "sess-1");
    assert_eq!(snapshot.version, 0);
    assert_eq!(snapshot.totals.llm_call_count, 0);
    assert!(snapshot.by_execution_binding.is_empty());
}

#[test]
fn response_performance_is_weighted_deduplicated_and_excludes_failed_calls() {
    use magi_app_server_protocol::ModelResponseTiming;
    let mut authority = UsageAuthority::new();
    for (id, ttft, tokens, duration, status) in [
        ("body-a", 200, Some(20), 1000, UsageCallStatus::Success),
        ("body-b", 600, Some(180), 3000, UsageCallStatus::Success),
        (
            "body-no-token-pair",
            400,
            None,
            2000,
            UsageCallStatus::Success,
        ),
        ("single-frame", 800, Some(10), 0, UsageCallStatus::Success),
        ("interrupted", 9000, Some(9999), 1, UsageCallStatus::Failed),
    ] {
        let mut call = make_call_record_input("perf-session", id, 200, 200);
        call.call_identity.transport = Some(UsageTransportIdentity {
            usage_reported: Some(true),
            request_fingerprint: "a".repeat(64),
            http_status: Some(200),
            response_timing: Some(ModelResponseTiming {
                response_duration_ms: Some(ttft + duration),
                first_content_ms: ttft,
                content_duration_ms: duration,
                text_output_tokens: tokens,
            }),
        });
        call.status = status;
        authority.append_call_record(call.clone());
        authority.append_call_record(call);
    }
    let snapshot = authority.get_session_snapshot("ws-1", "perf-session");
    let perf = &snapshot.totals.performance;
    assert_eq!(snapshot.totals.llm_call_count, 5);
    assert_eq!(perf.ttft_sample_count, 4);
    assert_eq!(perf.ttft_total_ms, 2000);
    assert_eq!(perf.tps_sample_count, 2);
    assert_eq!(perf.text_output_tokens, 200);
    assert_eq!(perf.content_duration_ms, 4000);
    assert_eq!(perf.response_sample_count, 4);
    assert_eq!(
        perf.response_output_tokens, 800,
        "all reported output participates, including unsplit reasoning"
    );
    assert_eq!(perf.response_duration_ms, 8000);
    assert_eq!(perf.last_response_tps, Some(250.0));
    assert_eq!(
        snapshot.by_execution_binding[0]
            .totals
            .performance
            .text_output_tokens,
        200
    );
    assert_eq!(
        authority
            .get_workspace_snapshot("ws-1")
            .totals
            .performance
            .ttft_sample_count,
        4
    );
    // Old receipts retain unknown performance and cache-write presence.
    let mut old = serde_json::to_value(&snapshot.totals).unwrap();
    old.as_object_mut().unwrap().remove("performance");
    old.as_object_mut()
        .unwrap()
        .remove("cacheWriteObservedCallCount");
    let old: UsageTotals = serde_json::from_value(old).unwrap();
    assert_eq!(old.performance.ttft_sample_count, 0);
    assert_eq!(old.cache_write_observed_call_count, 0);
    let mut old_performance = serde_json::to_value(perf).unwrap();
    for name in [
        "responseSampleCount",
        "responseOutputTokens",
        "responseDurationMs",
        "lastResponseTps",
    ] {
        old_performance.as_object_mut().unwrap().remove(name);
    }
    let restored: UsagePerformanceTotals = serde_json::from_value(old_performance).unwrap();
    assert_eq!(
        restored.response_sample_count, 0,
        "old body-only timing is not reinterpreted as complete response timing"
    );
    assert_eq!(restored.last_response_tps, None);
}

#[test]
fn token_presence_and_invalid_cache_do_not_become_zero_or_fabricated_hits() {
    let mut authority = UsageAuthority::new();
    for (id, reported) in [
        ("unknown", Some(false)),
        ("confirmed-zero", Some(true)),
        ("old-zero", None),
    ] {
        let mut call = make_call_record_input("observations", id, 0, 0);
        call.usage.cache_read_tokens = None;
        call.usage.cache_write_tokens = None;
        call.call_identity.transport = Some(UsageTransportIdentity {
            usage_reported: reported,
            response_timing: Some(magi_app_server_protocol::ModelResponseTiming {
                first_content_ms: 100,
                content_duration_ms: 200,
                text_output_tokens: None,
                response_duration_ms: Some(500),
            }),
            request_fingerprint: "f".repeat(64),
            http_status: Some(200),
        });
        authority.append_call_record(call);
        if reported == Some(true) {
            assert_eq!(
                authority
                    .get_session_snapshot("ws-1", "observations")
                    .totals
                    .performance
                    .last_response_tps,
                Some(0.0),
                "confirmed zero is a real zero-speed sample"
            );
        }
    }
    let totals = authority
        .get_session_snapshot("ws-1", "observations")
        .totals;
    assert_eq!(totals.llm_call_count, 3);
    assert_eq!(totals.token_observed_call_count, 1);
    assert_eq!(totals.cache_observed_call_count, 0);
    assert_eq!(
        totals.performance.response_sample_count, 1,
        "unknown usage is not a zero-speed sample"
    );
    assert_eq!(totals.performance.response_output_tokens, 0);
    assert_eq!(totals.performance.response_duration_ms, 500);
    assert_eq!(
        totals.performance.last_response_tps, None,
        "latest unknown response does not inherit an earlier speed"
    );

    let mut invalid = make_call_record_input("bad-cache", "call", 1000, 10);
    invalid.usage.cache_read_tokens = Some(2000);
    invalid.usage.cache_read_included_in_input = true;
    invalid.usage.cache_write_tokens = None;
    authority.append_call_record(invalid);
    let totals = authority.get_session_snapshot("ws-1", "bad-cache").totals;
    assert_eq!(totals.cache_observed_call_count, 0);
    assert_eq!(totals.cache_hit_call_count, 0);
    assert_eq!(totals.cache_read_tokens, 0);
    assert_eq!(totals.cache_observed_input_tokens, 0);
    assert_eq!(
        totals.net_input_tokens, 1000,
        "bad cache metadata cannot inflate complete input or erase ordinary input"
    );
}

#[test]
fn transport_usage_presence_distinguishes_new_explicit_zero_from_old_placeholders() {
    let mut receipt: magi_app_server_protocol::ModelTransportRecord = serde_json::from_value(serde_json::json!({
        "requestFingerprint":"a".repeat(64), "usage":{"inputTokens":0,"outputTokens":0,"cacheReadIncludedInInput":false}
    })).unwrap();
    assert_eq!(
        UsageTransportIdentity::from_receipt(&receipt).usage_reported,
        None
    );
    receipt.usage_observed = Some(true);
    assert_eq!(
        UsageTransportIdentity::from_receipt(&receipt).usage_reported,
        Some(true)
    );
    let restored = serde_json::from_value(serde_json::to_value(&receipt).unwrap()).unwrap();
    assert_eq!(
        UsageTransportIdentity::from_receipt(&restored).usage_reported,
        Some(true)
    );
    receipt.usage_observed = None;
    receipt.usage.as_mut().unwrap().output_tokens = 10;
    assert_eq!(
        UsageTransportIdentity::from_receipt(&receipt).usage_reported,
        Some(true)
    );
}
