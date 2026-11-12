#[test]
fn model_step_usage_context_freezes_sanitized_connection_and_account_identity() {
    use super::*;
    let settings = Arc::new(SettingsStore::new());
    let session = SessionId::new("safe-usage-binding");
    settings
        .set_section(
            "orchestrator",
            crate::main_model_sources::test_sources(
                "https://url-user:url-password@example.test/v1?api_key=url-token#fragment",
                "private-key",
                &["original-model", "later-model"],
            ),
        )
        .unwrap();
    settings
        .set_session_section(
            &session,
            "orchestrator",
            serde_json::json!({"sourceId":"custom/test","model":"original-model"}),
        )
        .unwrap();
    let frozen = model_step_usage_context(
        Some(&settings),
        &session_turn_model_usage_binding(true),
        &session,
    )
    .unwrap();
    magi_session_store::validate_model_usage_context(&frozen).unwrap();
    assert_eq!(frozen.model_config.base_url, "https://example.test/v1");
    assert!(frozen.model_config.account_fingerprint.is_some());
    let serialized = serde_json::to_string(&frozen).unwrap();
    for secret in [
        "url-user",
        "url-password",
        "url-token",
        "private-key",
        "fragment",
    ] {
        assert!(!serialized.contains(secret));
    }
    settings
        .set_session_section(
            &session,
            "orchestrator",
            serde_json::json!({"sourceId":"custom/test","model":"later-model"}),
        )
        .unwrap();
    assert_eq!(frozen.model_config.model, "original-model");
}
#[test]
fn attempt_usage_uses_physical_identities_and_replay_does_not_double_charge() {
    use super::*;
    use magi_session_store::{ModelTransportOutcome, ModelTransportRecord, ModelTransportUsage};
    let bus = InMemoryEventBus::new(16);
    let store = SessionStore::new();
    let session = SessionId::new("transport-ledger");
    let workspace = Some(WorkspaceId::new("workspace-transport"));
    let binding = auxiliary_model_usage_binding(UsagePhase::Integration);
    let receipts = vec![
        ModelTransportRecord {
            usage_observed: None,
            response_timing: None,
            observed_at: None,
            request_fingerprint: "a".repeat(64),
            http_status: Some(200),
            usage: Some(ModelTransportUsage {
                input_tokens: 11,
                output_tokens: 1,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cache_read_included_in_input: false,
            }),
            outcome: Some(ModelTransportOutcome::Failed),
        },
        ModelTransportRecord {
            usage_observed: None,
            response_timing: None,
            observed_at: None,
            request_fingerprint: "b".repeat(64),
            http_status: Some(200),
            usage: Some(ModelTransportUsage {
                input_tokens: 20,
                output_tokens: 2,
                cache_read_tokens: Some(10),
                cache_write_tokens: None,
                cache_read_included_in_input: true,
            }),
            outcome: Some(ModelTransportOutcome::Succeeded),
        },
    ];
    for _ in 0..2 {
        publish_model_usage_record_internal(
            &bus,
            &store,
            None,
            ModelUsageRecordInput {
                session_id: &session,
                workspace_id: &workspace,
                binding: &binding,
                call_id: "logical-call".into(),
                usage: None,
                context_breakdown: None,
                status: UsageCallStatus::Success,
                assignment_id: None,
                error_code: None,
            },
            PublishModelUsageOptions {
                explicit_model_config: Some(LlmConfig {
                    provider: "test".into(),
                    model: "test".into(),
                    base_url: "http://localhost".into(),
                    api_key: None,
                    account_fingerprint: None,
                    reasoning_effort: None,
                }),
                expected_turn_id: Some("original-turn"),
                context_window_override: None,
                transport_records: Some(&receipts),
            },
        );
    }
    let mut authority = magi_usage_authority::UsageAuthority::new();
    let events = bus.snapshot().recent_events;
    assert_eq!(events.len(), 2);
    for event in events {
        let record: UsageCallRecordInput = serde_json::from_value(event.payload).unwrap();
        assert_eq!(
            record.call_identity.parent_call_id.as_deref(),
            Some("logical-call")
        );
        assert_eq!(record.turn_id.as_deref(), Some("original-turn"));
        assert_eq!(
            record.call_identity.transport.as_ref().unwrap().http_status,
            Some(200)
        );
        authority.append_call_record(record);
    }
    let snapshot = authority.get_session_snapshot("workspace-transport", session.as_str());
    assert_eq!(snapshot.totals.llm_call_count, 2);
    assert_eq!(snapshot.totals.raw_input_tokens, 31);
    assert_eq!(snapshot.totals.raw_output_tokens, 3);
    assert_eq!(snapshot.totals.total_tokens, 24);
    assert_eq!(snapshot.totals.cache_read_tokens, 10);
    assert_eq!(snapshot.totals.cache_observed_call_count, 1);
    assert_eq!(snapshot.totals.failure_count, 1);
}
use super::*;
use magi_event_bus::InMemoryEventBus;
use magi_session_store::SessionStore;
use magi_usage_authority::UsageCallStatus;
use serde_json::json;

fn model_response(payload: serde_json::Value) -> ModelResponse {
    ModelResponse::from_chat_payload(
        serde_json::from_value(payload).expect("测试模型响应必须符合统一响应结构"),
    )
}

#[test]
fn session_turn_model_usage_binding_respects_phase() {
    let planning = session_turn_model_usage_binding(false);
    let execution = session_turn_model_usage_binding(true);

    assert_eq!(planning.template_id, "orchestrator");
    assert_eq!(planning.engine_id, "orchestrator");
    assert!(matches!(planning.role, UsageSourceRole::Orchestrator));
    assert!(matches!(planning.phase, UsagePhase::Planning));
    assert!(matches!(execution.phase, UsagePhase::Execution));
}

#[test]
fn auxiliary_cache_is_partitioned_by_real_purpose_and_drafts_have_no_identity() {
    struct Capture(std::sync::Mutex<Vec<Option<String>>>);
    impl ModelBridgeClient for Capture {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            magi_bridge_client::native_executor_capabilities()
        }
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            self.0.lock().unwrap().push(request.prompt_cache_key);
            Ok(ModelResponse::completed("ok"))
        }
        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }
    let client = Arc::new(Capture(std::sync::Mutex::new(Vec::new())));
    let store = SessionStore::new();
    let session = SessionId::new("purpose-cache-session");
    store.create_session(session.clone(), "cache").unwrap();
    let bus = InMemoryEventBus::new(32);
    let purposes = [
        AuxiliaryModelPurpose::SessionTitle,
        AuxiliaryModelPurpose::PromptEnhance,
        AuxiliaryModelPurpose::KnowledgeExtraction,
        AuxiliaryModelPurpose::SessionMemoryExtraction,
    ];
    for (index, purpose) in purposes
        .into_iter()
        .chain(purposes)
        .chain([purposes[0]])
        .enumerate()
    {
        invoke_auxiliary_model_with_usage(
            client.clone(),
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: Some("must-be-replaced".into()),
                provider: "test".into(),
                prompt: format!("input {index}"),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            AuxiliaryModelUsageContext {
                event_bus: &bus,
                session_store: &store,
                settings_store: None,
                session_id: (index < 8).then_some(&session),
                workspace_id: &None,
                call_id: format!("call-{index}"),
                phase: UsagePhase::Integration,
                purpose,
                expected_turn_id: None,
            },
        )
        .unwrap();
    }
    let keys = client.0.lock().unwrap();
    assert_eq!(keys[..4], keys[4..8]);
    assert_eq!(
        keys[..5]
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
    assert!(keys[8].is_none());
}

#[test]
fn deepseek_cache_hit_tokens_are_included_in_prompt_tokens() {
    let usage = usage_tokens_from_payload(Some(&json!({
            "prompt_tokens":100, "completion_tokens":10, "prompt_cache_hit_tokens":60, "prompt_cache_miss_tokens":40
        }))).unwrap();
    assert_eq!(usage.cache_read_tokens, Some(60));
    assert!(usage.cache_read_included_in_input);
    assert_eq!(provider_context_tokens_from_usage(&usage), 100);
}

#[test]
fn cache_only_usage_is_recorded_without_fabricating_uncached_input() {
    let usage = usage_tokens_from_payload(Some(&json!({
        "input_tokens": 0, "output_tokens": 0,
        "cache_read_input_tokens": 1000, "cache_creation_input_tokens": 200,
    })))
    .expect("cache-only usage");
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.cache_read_tokens, Some(1000));
    assert_eq!(provider_context_tokens_from_usage(&usage), 1200);
    let zero = usage_tokens_from_payload(Some(&json!({
        "input_tokens": 0, "output_tokens": 0,
        "input_tokens_details": { "cached_tokens": 0 }
    })))
    .expect("explicit zero cache observation");
    assert_eq!(zero.cache_read_tokens, Some(0));
    assert!(usage_tokens_from_payload(Some(&json!({}))).is_none());
}

#[test]
fn context_usage_runtime_tracker_publishes_prefill_and_streaming_estimates() {
    let event_bus = InMemoryEventBus::new(8);
    let settings_store = SettingsStore::new();
    let session_id = SessionId::new("session-context-runtime");
    let workspace_id = Some(WorkspaceId::new("workspace-context-runtime"));
    let tracker = ContextUsageRuntimeTracker::start(ContextUsageRuntimeTrackerInput {
        event_bus: &event_bus,
        settings_store: Some(&settings_store),
        session_id: &session_id,
        workspace_id: &workspace_id,
        turn_id: Some("turn-context-runtime"),
        call_id: "call-context-runtime",
        resolved_model: "gpt-5.6-luna",
        context_window_tokens: 256_000,
        context_breakdown: ContextUsageBreakdown {
            conversation_tokens: 1_000,
            ..Default::default()
        },
        thread_id: None,
        model_provider: None,
        binding_revision: 0,
        checkpoint_generation: 0,
    });

    tracker.observe_accumulated_output("圆环实时增长", "正在思考");

    let events = event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .filter(|event| event.event_type == "session.context.pressure.updated")
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].payload["phase"], json!("prefill"));
    assert_eq!(events[0].payload["measurement"], json!("estimated"));
    assert_eq!(events[0].payload["projected_request_tokens"], json!(1_000));
    assert_eq!(
        events[0].payload["context_window_limit_tokens"],
        json!(256_000)
    );
    assert_eq!(events[1].payload["phase"], json!("streaming"));
    assert_eq!(events[1].payload["measurement"], json!("estimated"));
    assert!(
        events[1].payload["projected_request_tokens"]
            .as_u64()
            .unwrap_or_default()
            > 1_000
    );
    assert_eq!(events[1].payload["call_id"], json!("call-context-runtime"));
    assert_eq!(events[1].payload["turn_id"], json!("turn-context-runtime"));
}

#[test]
fn usage_tokens_from_payload_prefers_prompt_and_completion_fields() {
    let usage = usage_tokens_from_payload(Some(&json!({
        "prompt_tokens": 3,
        "completion_tokens": 7,
        "prompt_tokens_details": { "cached_tokens": 2 },
        "cache_creation_input_tokens": 5
    })))
    .expect("usage tokens");

    assert_eq!(usage.input_tokens, 3);
    assert_eq!(usage.output_tokens, 7);
    assert_eq!(usage.cache_read_tokens, Some(2));
    assert_eq!(usage.cache_write_tokens, Some(5));
    assert!(usage.cache_read_included_in_input);
}

#[test]
fn usage_tokens_from_payload_reads_responses_input_details() {
    let usage = usage_tokens_from_payload(Some(&json!({
        "input_tokens": 11,
        "output_tokens": 13,
        "total_tokens": 31,
        "input_tokens_details": { "cached_tokens": 4 }
    })))
    .expect("usage tokens");

    assert_eq!(usage.input_tokens, 11);
    assert_eq!(usage.output_tokens, 13);
    assert_eq!(usage.total_tokens, Some(31));
    assert_eq!(usage.cache_read_tokens, Some(4));
    assert!(usage.cache_read_included_in_input);
}

#[test]
fn usage_tokens_from_payload_preserves_zero_cache_reports() {
    let usage = usage_tokens_from_payload(Some(&json!({
        "input_tokens": 11,
        "output_tokens": 13,
        "input_tokens_details": { "cached_tokens": 0 },
        "cache_creation_input_tokens": 0
    })))
    .expect("usage tokens");

    assert_eq!(usage.input_tokens, 11);
    assert_eq!(usage.output_tokens, 13);
    assert_eq!(usage.cache_read_tokens, Some(0));
    assert_eq!(usage.cache_write_tokens, Some(0));
    assert!(usage.cache_read_included_in_input);
}

#[test]
fn usage_tokens_from_payload_keeps_anthropic_cache_separate() {
    let usage = usage_tokens_from_payload(Some(&json!({
        "input_tokens": 11,
        "output_tokens": 13,
        "cache_read_input_tokens": 4,
        "cache_creation_input_tokens": 6
    })))
    .expect("usage tokens");

    assert_eq!(usage.input_tokens, 11);
    assert_eq!(usage.output_tokens, 13);
    assert_eq!(usage.cache_read_tokens, Some(4));
    assert_eq!(usage.cache_write_tokens, Some(6));
    assert!(!usage.cache_read_included_in_input);
}

#[test]
fn usage_tokens_from_payload_honors_bridge_camel_case_inclusion_flag() {
    let usage = usage_tokens_from_payload(Some(&json!({
        "inputTokens": 11,
        "outputTokens": 13,
        "cacheReadTokens": 4,
        "cacheReadIncludedInInput": true
    })))
    .expect("usage tokens");

    assert_eq!(usage.input_tokens, 11);
    assert_eq!(usage.output_tokens, 13);
    assert_eq!(usage.cache_read_tokens, Some(4));
    assert!(usage.cache_read_included_in_input);
}

#[test]
fn publish_model_usage_record_emits_usage_event() {
    let event_bus = InMemoryEventBus::new(8);
    let settings_store = Arc::new(SettingsStore::new());
    let session_id = SessionId::new("session-1");
    settings_store
        .set_section(
            "orchestrator",
            crate::main_model_sources::test_sources(
                "https://example.test",
                "secret-success-call-key",
                &["gpt-5.6-sol"],
            ),
        )
        .unwrap();
    settings_store
        .set_session_section(
            &session_id,
            "orchestrator",
            json!({
                "sourceId": "custom/test",
                "model": "gpt-5.6-sol",
                "reasoningEffort": "high"
            }),
        )
        .unwrap();
    let binding = session_turn_model_usage_binding(true);
    let workspace_id = Some(WorkspaceId::new("workspace-1"));

    let session_store = SessionStore::from_state_for_test(magi_session_store::SessionStoreState {
            canonical_turns: vec![serde_json::from_value(json!({
                "sessionId": session_id, "turnId": "usage-turn", "turnSeq": 1,
                "acceptedAt": 1, "status": "running", "items": [],
                "modelSteps": [{
                    "stepId": "call-1", "threadId": "original-execution-thread", "checkpointGeneration": 7,
                    "attempts": [{"attemptId": "call-1:attempt:1", "startedAt": 1, "settledAt": 2, "status": "response_received"}],
                    "requestFingerprint": "a".repeat(64), "provider": "test", "messageCount": 1,
                    "toolCount": 0, "inputRequestIds": [], "startedAt": 1, "settledAt": 2,
                    "status": "response_received", "responseFingerprint": "b".repeat(64)
                }]
            })).unwrap()],
            ..Default::default()
        });
    // 当前主线程没有检查点；完成事件仍应使用原 Step 的线程与代际。
    session_store.ensure_session_mission(&session_id, UtcMillis(3), || {
        magi_core::MissionId::new("usage-mission")
    });
    publish_model_usage_record_for_turn(
        &event_bus,
        &session_store,
        Some(&settings_store),
        Some("usage-turn"),
        ModelUsageRecordInput {
            context_breakdown: None,
            session_id: &session_id,
            workspace_id: &workspace_id,
            binding: &binding,
            call_id: "call-1".to_string(),
            usage: Some(&json!({"prompt_tokens": 3, "completion_tokens": 7})),
            status: UsageCallStatus::Success,
            assignment_id: Some("assignment-1".to_string()),
            error_code: None,
        },
    );

    let snapshot = event_bus.snapshot();
    assert_eq!(snapshot.recent_events.len(), 2);
    let usage_event = snapshot
        .recent_events
        .iter()
        .find(|event| event.event_type == "model.usage.recorded")
        .expect("usage event should exist");
    let context_event = snapshot
        .recent_events
        .iter()
        .find(|event| event.event_type == "session.context.pressure.updated")
        .expect("authoritative context event should exist");
    assert_eq!(usage_event.category, magi_event_bus::EventCategory::Usage);
    assert_eq!(usage_event.workspace_id.as_ref(), workspace_id.as_ref());
    assert_eq!(usage_event.payload["workspaceId"], json!("workspace-1"));
    assert_eq!(
        usage_event.payload["modelConfig"]["model"],
        json!("gpt-5.6-sol")
    );
    assert_eq!(
        usage_event.payload["modelConfig"]["reasoningEffort"],
        json!("high")
    );
    assert!(usage_event.payload["modelConfig"].get("apiKey").is_none());
    assert!(
        !usage_event
            .payload
            .to_string()
            .contains("secret-success-call-key")
    );
    assert_eq!(
        context_event.payload["thread_id"],
        json!("original-execution-thread")
    );
    assert_eq!(context_event.payload["checkpoint_generation"], json!(7));
    assert_eq!(context_event.payload["measurement"], json!("authoritative"));
    assert_eq!(context_event.payload["phase"], json!("completed"));
    assert_eq!(context_event.payload["projected_request_tokens"], json!(3));
    assert_eq!(
        context_event.payload["resolved_model"],
        json!("gpt-5.6-sol")
    );
}

#[test]
fn fixed_turn_context_window_is_kept_for_authoritative_usage_snapshot() {
    let event_bus = InMemoryEventBus::new(8);
    let session_store = SessionStore::new();
    let settings_store = Arc::new(SettingsStore::new());
    let session_id = SessionId::new("session-fixed-context-window");
    settings_store
        .set_section(
            "orchestrator",
            crate::main_model_sources::test_sources(
                "https://example.test",
                "secret",
                &["gpt-fixed-window"],
            ),
        )
        .unwrap();
    settings_store
        .set_session_section(
            &session_id,
            "orchestrator",
            json!({"sourceId": "custom/test", "model": "gpt-fixed-window"}),
        )
        .unwrap();
    let binding = session_turn_model_usage_binding(true);
    let workspace_id = None;

    let request_breakdown = ContextUsageBreakdown {
        conversation_tokens: 100,
        system_instruction_tokens: 25,
        ..Default::default()
    };
    let tracker = ContextUsageRuntimeTracker::start(ContextUsageRuntimeTrackerInput {
        event_bus: &event_bus,
        settings_store: Some(&settings_store),
        session_id: &session_id,
        workspace_id: &workspace_id,
        turn_id: Some("turn-fixed-context-window"),
        call_id: "call-fixed-context-window",
        resolved_model: "gpt-fixed-window",
        context_window_tokens: 256_000,
        context_breakdown: request_breakdown.clone(),
        thread_id: None,
        model_provider: Some("configured".into()),
        binding_revision: 0,
        checkpoint_generation: 0,
    });
    tracker.observe_accumulated_output(&"output".repeat(100), "thinking");
    publish_model_usage_record_for_turn_with_context_window(
        &event_bus,
        &session_store,
        Some(&settings_store),
        Some("turn-fixed-context-window"),
        256_000,
        ModelUsageRecordInput {
            context_breakdown: Some(request_breakdown.clone()),
            session_id: &session_id,
            workspace_id: &workspace_id,
            binding: &binding,
            call_id: "call-fixed-context-window".to_string(),
            usage: Some(&json!({"prompt_tokens": 42, "completion_tokens": 7})),
            status: UsageCallStatus::Success,
            assignment_id: None,
            error_code: None,
        },
    );

    let event = event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .find(|event| {
            event.event_type == "session.context.pressure.updated"
                && event.payload["phase"] == "completed"
        })
        .expect("固定窗口必须写入 authoritative pressure 事件");
    assert_eq!(event.payload["context_window_limit_tokens"], json!(256_000));
    assert_eq!(event.payload["projected_request_tokens"], json!(42));
    assert_eq!(event.payload["context_breakdown"], json!(request_breakdown));
    let restored = event_bus
        .with_audit_usage_ledger(|ledger| {
            latest_usage_observation_for_session(&ledger.usage_entries, session_id.as_str())
        })
        .unwrap();
    assert_eq!(restored.context_breakdown, Some(request_breakdown));
}

struct SuccessfulAuxiliaryClient;

impl ModelBridgeClient for SuccessfulAuxiliaryClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, _request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        Ok(model_response(json!({
            "content": "辅助模型已完成",
            "usage": {
                "prompt_tokens": 17,
                "completion_tokens": 9,
                "total_tokens": 26
            }
        })))
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        _on_delta: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke(request)
    }
}

#[test]
fn invoke_auxiliary_model_with_usage_records_success_and_auxiliary_config() {
    let event_bus = InMemoryEventBus::new(8);
    let session_store = SessionStore::new();
    let settings_store = Arc::new(SettingsStore::new());
    settings_store
        .set_section(
            "auxiliary",
            json!({
                "baseUrl": "https://auxiliary.example.test/v1",
                "apiProtocol": "openai_chat",
                "apiKey": "secret-auxiliary-success-key",
                "model": "auxiliary-success-model"
            }),
        )
        .unwrap();
    let session_id = SessionId::new("session-auxiliary-success");
    let workspace_id = Some(WorkspaceId::new("workspace-auxiliary-success"));

    let response = invoke_auxiliary_model_with_usage(
        Arc::new(SuccessfulAuxiliaryClient),
        ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "openai-compatible".to_string(),
            prompt: "测试辅助模型用量记录".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        },
        AuxiliaryModelUsageContext {
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: Some(&settings_store),
            session_id: Some(&session_id),
            workspace_id: &workspace_id,
            call_id: "auxiliary-success-call".to_string(),
            phase: UsagePhase::Integration,
            purpose: AuxiliaryModelPurpose::KnowledgeExtraction,
            expected_turn_id: None,
        },
    )
    .expect("辅助模型成功调用不应失败");

    assert_eq!(response.content.as_deref(), Some("辅助模型已完成"));
    let event = event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .find(|event| event.event_type == "model.usage.recorded")
        .expect("辅助模型成功调用必须写入用量账本");
    assert_eq!(event.payload["status"], json!("success"));
    assert_eq!(
        event.payload["executionBinding"]["role"],
        json!("auxiliary")
    );
    assert_eq!(
        event.payload["modelConfig"]["model"],
        json!("auxiliary-success-model")
    );
    assert_eq!(event.payload["usage"]["inputTokens"], json!(17));
    assert_eq!(event.payload["usage"]["outputTokens"], json!(9));
    assert_eq!(event.payload["usage"]["totalTokens"], json!(26));
    assert!(
        !event
            .payload
            .to_string()
            .contains("secret-auxiliary-success-key")
    );
}

#[test]
fn auxiliary_usage_inherits_orchestrator_identity_when_auxiliary_is_unconfigured() {
    let event_bus = InMemoryEventBus::new(8);
    let session_store = SessionStore::new();
    let settings_store = Arc::new(SettingsStore::new());
    let session_id = SessionId::new("session-auxiliary-inherits-orchestrator");
    settings_store
        .set_section(
            "orchestrator",
            crate::main_model_sources::test_sources(
                "https://orchestrator.example.test/v1",
                "secret-orchestrator-key",
                &["orchestrator-inherited-model"],
            ),
        )
        .unwrap();
    settings_store
        .set_session_section(
            &session_id,
            "orchestrator",
            json!({"sourceId": "custom/test", "model": "orchestrator-inherited-model"}),
        )
        .unwrap();
    let binding = auxiliary_model_usage_binding(UsagePhase::Integration);
    let workspace_id = Some(WorkspaceId::new(
        "workspace-auxiliary-inherits-orchestrator",
    ));

    publish_model_usage_record(
        &event_bus,
        &session_store,
        Some(&settings_store),
        ModelUsageRecordInput {
            context_breakdown: None,
            session_id: &session_id,
            workspace_id: &workspace_id,
            binding: &binding,
            call_id: "auxiliary-inherited-call".to_string(),
            usage: Some(&json!({"input_tokens": 5, "output_tokens": 3})),
            status: UsageCallStatus::Success,
            assignment_id: None,
            error_code: None,
        },
    );

    let event = event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .find(|event| event.event_type == "model.usage.recorded")
        .expect("继承 orchestrator 的辅助调用必须写入用量账本");
    assert_eq!(
        event.payload["executionBinding"]["role"],
        json!("auxiliary")
    );
    assert_eq!(event.payload["callIdentity"]["phase"], json!("integration"));
    assert_eq!(
        event.payload["modelConfig"]["model"],
        json!("orchestrator-inherited-model")
    );
}

#[test]
fn publish_model_usage_record_keeps_successful_call_without_token_usage() {
    let event_bus = InMemoryEventBus::new(8);
    let session_store = SessionStore::new();
    let settings_store = Arc::new(SettingsStore::new());
    let session_id = SessionId::new("session-success-without-usage");
    settings_store
        .set_section(
            "auxiliary",
            json!({
                "baseUrl": "https://example.test",
                "apiProtocol": "openai_chat",
                "model": "auxiliary-no-usage-model"
            }),
        )
        .unwrap();
    let binding = auxiliary_model_usage_binding(UsagePhase::Planning);
    let workspace_id = Some(WorkspaceId::new("workspace-success-without-usage"));

    publish_model_usage_record(
        &event_bus,
        &session_store,
        Some(&settings_store),
        ModelUsageRecordInput {
            context_breakdown: None,
            session_id: &session_id,
            workspace_id: &workspace_id,
            binding: &binding,
            call_id: "call-success-without-usage".to_string(),
            usage: None,
            status: UsageCallStatus::Success,
            assignment_id: None,
            error_code: None,
        },
    );

    let event = event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .find(|event| event.event_type == "model.usage.recorded")
        .expect("没有 token 的成功调用也必须保留调用统计");
    assert_eq!(event.payload["status"], json!("success"));
    assert_eq!(event.payload["usage"]["inputTokens"], json!(0));
    assert_eq!(event.payload["usage"]["outputTokens"], json!(0));
    assert_eq!(event.payload["usage"]["totalTokens"], json!(0));
}

#[test]
fn image_generation_usage_records_calls_and_optional_provider_tokens() {
    let event_bus = InMemoryEventBus::new(8);
    let session_store = SessionStore::new();
    let settings_store = Arc::new(SettingsStore::new());
    let session_id = SessionId::new("session-image-usage");
    settings_store
        .set_section(
            "imageGeneration",
            json!({
                "baseUrl": "https://images.example.test/v1",
                "apiProtocol": "openai_chat",
                "apiKey": "secret-image-key",
                "model": "gpt-image-test"
            }),
        )
        .unwrap();
    let binding = image_generation_model_usage_binding();
    let workspace_id = Some(WorkspaceId::new("workspace-image-usage"));
    let provider_usage = json!({
        "input_tokens": 18,
        "output_tokens": 32,
        "total_tokens": 50
    });

    publish_model_usage_record(
        &event_bus,
        &session_store,
        Some(&settings_store),
        ModelUsageRecordInput {
            context_breakdown: None,
            session_id: &session_id,
            workspace_id: &workspace_id,
            binding: &binding,
            call_id: "image-call-1".to_string(),
            usage: Some(&provider_usage),
            status: UsageCallStatus::Success,
            assignment_id: None,
            error_code: None,
        },
    );

    let event = event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .find(|event| event.event_type == "model.usage.recorded")
        .expect("图片模型调用必须进入统一模型用量账本");
    assert_eq!(
        event.payload["executionBinding"]["role"],
        json!("image_generation")
    );
    assert_eq!(
        event.payload["executionBinding"]["templateId"],
        json!("imageGeneration")
    );
    assert_eq!(
        event.payload["modelConfig"]["model"],
        json!("gpt-image-test")
    );
    assert_eq!(event.payload["usage"]["inputTokens"], json!(18));
    assert_eq!(event.payload["usage"]["outputTokens"], json!(32));
    assert_eq!(event.payload["usage"]["totalTokens"], json!(50));
}

#[test]
fn publish_model_usage_record_keeps_failed_call_without_token_usage() {
    let event_bus = InMemoryEventBus::new(8);
    let session_store = SessionStore::new();
    let settings_store = Arc::new(SettingsStore::new());
    let session_id = SessionId::new("session-failed-call");
    settings_store
        .set_section(
            "orchestrator",
            crate::main_model_sources::test_sources(
                "https://example.test",
                "secret-failed-call-key",
                &["model-failed-call"],
            ),
        )
        .unwrap();
    settings_store
        .set_session_section(
            &session_id,
            "orchestrator",
            json!({"sourceId": "custom/test", "model": "model-failed-call"}),
        )
        .unwrap();
    let binding = session_turn_model_usage_binding(true);
    let workspace_id = Some(WorkspaceId::new("workspace-failed-call"));

    publish_model_usage_record(
        &event_bus,
        &session_store,
        Some(&settings_store),
        ModelUsageRecordInput {
            context_breakdown: None,
            session_id: &session_id,
            workspace_id: &workspace_id,
            binding: &binding,
            call_id: "call-failed".to_string(),
            usage: None,
            status: UsageCallStatus::Failed,
            assignment_id: None,
            error_code: Some("model_stream_interrupted".to_string()),
        },
    );

    let snapshot = event_bus.snapshot();
    assert_eq!(snapshot.recent_events.len(), 1);
    assert_eq!(snapshot.recent_events[0].payload["status"], json!("failed"));
    assert_eq!(
        snapshot.recent_events[0].payload["errorCode"],
        json!("model_stream_interrupted")
    );
    assert_eq!(snapshot.recent_events[0].payload["usage"]["inputTokens"], 0);
    assert_eq!(
        snapshot.recent_events[0].payload["usage"]["outputTokens"],
        0
    );
    assert!(
        snapshot.recent_events[0].payload["modelConfig"]
            .get("apiKey")
            .is_none()
    );
    assert!(
        snapshot.recent_events[0].payload["modelConfig"]["accountFingerprint"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        !snapshot.recent_events[0]
            .payload
            .to_string()
            .contains("secret-failed-call-key")
    );
}

#[test]
fn publish_model_usage_record_emits_personal_session_usage_without_workspace() {
    let event_bus = InMemoryEventBus::new(8);
    let session_store = SessionStore::new();
    let settings_store = Arc::new(SettingsStore::new());
    let session_id = SessionId::new("session-1");
    settings_store
        .set_section(
            "orchestrator",
            crate::main_model_sources::test_sources(
                "https://example.test",
                "",
                &["gpt-session-test"],
            ),
        )
        .unwrap();
    settings_store
        .set_session_section(
            &session_id,
            "orchestrator",
            json!({
                "sourceId": "custom/test",
                "model": "gpt-session-test"
            }),
        )
        .unwrap();
    let binding = session_turn_model_usage_binding(true);

    publish_model_usage_record(
        &event_bus,
        &session_store,
        Some(&settings_store),
        ModelUsageRecordInput {
            context_breakdown: None,
            session_id: &session_id,
            workspace_id: &None,
            binding: &binding,
            call_id: "call-1".to_string(),
            usage: Some(&json!({"prompt_tokens": 3, "completion_tokens": 7})),
            status: UsageCallStatus::Success,
            assignment_id: Some("assignment-1".to_string()),
            error_code: None,
        },
    );

    let snapshot = event_bus.snapshot();
    let usage_event = snapshot
        .recent_events
        .iter()
        .find(|event| event.event_type == "model.usage.recorded")
        .expect("personal session usage should be recorded");
    assert!(usage_event.workspace_id.is_none());
    assert!(usage_event.payload.get("workspaceId").is_none());
    assert_eq!(usage_event.payload["sessionId"], json!(session_id.as_str()));
    assert!(
        snapshot
            .recent_events
            .iter()
            .any(|event| event.event_type == "session.context.pressure.updated"),
        "personal session should publish authoritative context usage"
    );
}
