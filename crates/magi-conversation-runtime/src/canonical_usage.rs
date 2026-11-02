//! canonical Step 是原生调用的用量事实；实时结算与重启补账使用同一投影。
use magi_core::{SessionId, WorkspaceId};
use magi_event_bus::InMemoryEventBus;
use magi_session_store::{ModelStep, ModelStepStatus, ModelTransportOutcome, SessionStore};
use magi_usage_authority::{
    ExecutionBindingIdentity, LlmConfig, UsageCallIdentity, UsageCallRecordInput, UsageCallStatus,
    UsageTokenInput, UsageTransportIdentity,
};

pub fn model_step_usage_records(
    session_id: &SessionId,
    turn_id: &str,
    step: &ModelStep,
) -> Vec<UsageCallRecordInput> {
    if step.status == ModelStepStatus::Started {
        return Vec::new();
    }
    let Some(context) = step
        .binding
        .as_ref()
        .and_then(|binding| binding.usage_context.as_ref())
    else {
        return Vec::new();
    };
    step.attempts
        .iter()
        .enumerate()
        .filter_map(|(index, attempt)| {
            Some(transport_usage_record(TransportUsageInput {
                context,
                session_id,
                turn_id: Some(turn_id),
                logical_call_id: &step.step_id,
                attempt_id: &attempt.attempt_id,
                transport: attempt.transport.as_ref()?,
                confirmed_response: index + 1 == step.attempts.len()
                    && step.status == ModelStepStatus::ResponseReceived,
                fallback_timestamp: attempt.settled_at.map_or(step.started_at.0, |at| at.0),
            }))
        })
        .collect()
}

pub(crate) struct TransportUsageInput<'a> {
    pub context: &'a magi_app_server_protocol::ModelUsageContext,
    pub session_id: &'a SessionId,
    pub turn_id: Option<&'a str>,
    pub logical_call_id: &'a str,
    pub attempt_id: &'a str,
    pub transport: &'a magi_session_store::ModelTransportRecord,
    pub confirmed_response: bool,
    pub fallback_timestamp: u64,
}

pub(crate) fn transport_usage_record(input: TransportUsageInput<'_>) -> UsageCallRecordInput {
    let TransportUsageInput {
        context,
        session_id,
        turn_id,
        logical_call_id,
        attempt_id,
        transport,
        confirmed_response,
        fallback_timestamp,
    } = input;
    let model_config: LlmConfig = serde_json::from_value(
        serde_json::to_value(&context.model_config).expect("usage config serializes"),
    )
    .expect("validated model usage context");
    let role =
        serde_json::from_value(serde_json::to_value(&context.role).expect("role serializes"))
            .expect("schema role matches authority");
    let phase =
        serde_json::from_value(serde_json::to_value(&context.phase).expect("phase serializes"))
            .expect("schema phase matches authority");
    let scope = context.workspace_id.as_deref().unwrap_or("personal");
    let status = match transport.outcome {
        Some(ModelTransportOutcome::Cancelled) => UsageCallStatus::Cancelled,
        Some(ModelTransportOutcome::Succeeded) if confirmed_response => UsageCallStatus::Success,
        _ => UsageCallStatus::Failed,
    };
    UsageCallRecordInput {
        workspace_id: context.workspace_id.clone(),
        session_id: session_id.to_string(),
        turn_id: turn_id.map(str::to_string),
        dispatch_wave_id: None,
        assignment_id: context.assignment_id.clone(),
        event_id: Some(format!("model-usage:{scope}:{session_id}:{attempt_id}")),
        timestamp: Some(transport.observed_at.unwrap_or(fallback_timestamp)),
        execution_binding: ExecutionBindingIdentity {
            template_id: context.template_id.clone(),
            engine_id: context.engine_id.clone(),
            binding_revision: context.binding_revision,
            role,
        },
        model_config,
        call_identity: UsageCallIdentity {
            call_id: attempt_id.into(),
            parent_call_id: Some(logical_call_id.into()),
            source: role,
            phase,
            transport: Some(UsageTransportIdentity::from_receipt(transport)),
        },
        usage: transport
            .usage
            .as_ref()
            .map(|usage| UsageTokenInput {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                total_tokens: None,
                cache_read_tokens: usage.cache_read_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                cache_read_included_in_input: usage.cache_read_included_in_input,
            })
            .unwrap_or(UsageTokenInput {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: Some(0),
                cache_read_tokens: None,
                cache_write_tokens: None,
                cache_read_included_in_input: false,
            }),
        status,
        error_code: (status != UsageCallStatus::Success).then(|| {
            transport
                .http_status
                .filter(|status| *status >= 400)
                .map(|status| format!("provider_http_{status}"))
                .unwrap_or_else(|| "model_response_unconfirmed".into())
        }),
    }
}

pub fn publish_model_step_usage(
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    turn_id: &str,
    step: &ModelStep,
) {
    for input in model_step_usage_records(session_id, turn_id, step)
        .into_iter()
        .filter(|_| step.status != ModelStepStatus::Started)
    {
        crate::usage_recording::publish_usage_call_input(
            event_bus,
            &input
                .workspace_id
                .as_ref()
                .map(|id| WorkspaceId::new(id.clone())),
            session_id,
            &input,
        );
    }
}

/// 只修复投影，不调用模型、工具或重放用户输入；重置/保留期限在账本追加锁内统一裁决。
pub fn recover_model_usage_projections(
    event_bus: &InMemoryEventBus,
    store: &SessionStore,
) -> magi_core::DomainResult<usize> {
    let steps = store.model_usage_projection_steps();
    for source in &steps {
        if source.step.status == ModelStepStatus::Started {
            store.settle_model_step(
                &source.session_id,
                &source.turn_id,
                &source.step.step_id,
                None,
                None,
                None,
            )?;
        }
    }
    let before = event_bus.audit_usage_ledger_status().usage_count;
    for source in store.model_usage_projection_steps() {
        publish_model_step_usage(event_bus, &source.session_id, &source.turn_id, &source.step);
    }
    crate::auxiliary_usage::recover(event_bus)?;
    Ok(event_bus
        .audit_usage_ledger_status()
        .usage_count
        .saturating_sub(before))
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{EventId, ThreadId, UtcMillis};
    use magi_event_bus::EventEnvelope;
    use magi_session_store::{
        ActiveExecutionTurn, ModelTransportRecord, ModelTransportUsage, TimelineEntryInput,
        TimelineEntryKind,
    };
    use serde_json::json;

    fn recorded_call(settled: bool, observed_at: u64) -> (SessionStore, SessionId) {
        recorded_call_for_role(settled, observed_at, "orchestrator")
    }

    fn recorded_call_for_role(
        settled: bool,
        observed_at: u64,
        role: &str,
    ) -> (SessionStore, SessionId) {
        let store = SessionStore::new();
        let session = SessionId::new("usage-recovery");
        store
            .create_session(session.clone(), "usage recovery")
            .unwrap();
        store
            .accept_current_turn_with_timeline_entry(
                session.clone(),
                TimelineEntryInput::new(
                    "timeline",
                    TimelineEntryKind::UserMessage,
                    "work",
                    UtcMillis(observed_at.saturating_sub(10)),
                ),
                ActiveExecutionTurn {
                    turn_id: "usage-turn".into(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(observed_at.saturating_sub(10)),
                    completed_at: None,
                    status: "running".into(),
                    user_message: Some("work".into()),
                    items: vec![],
                },
            )
            .unwrap();
        let step: ModelStep = serde_json::from_value(json!({
            "stepId":"usage-step", "attempts":[{"attemptId":"usage-step:attempt:1", "startedAt":observed_at, "status":"started", "settledAt":null}],
            "threadId":ThreadId::new("usage-thread"), "checkpointGeneration":0, "requestFingerprint":"a".repeat(64),
            "provider":"configured", "messageCount":1, "toolCount":0, "inputRequestIds":[], "startedAt":observed_at,
            "settledAt":null, "status":"started", "responseFingerprint":null,
            "binding":{"model":"original-model", "provider":"configured", "bindingRevision":1,"settingsRevision":1,
                "usageContext":{"templateId":role,"engineId":role,"bindingRevision":1,
                    "role":role,"phase":"execution","assignmentId":"original-lease",
                    "modelConfig":{"provider":"configured","model":"original-model","baseUrl":"https://original.test/v1","accountFingerprint":"1234567890abcdef"}}}
        })).unwrap();
        store
            .begin_model_step(&session, "usage-turn", step)
            .unwrap();
        for index in 0..2 {
            // Intermediate snapshots can over-report tokens/cache; persist the final correction.
            store
                .record_model_step_transport(
                    &session,
                    "usage-turn",
                    "usage-step",
                    ModelTransportRecord {
                        usage_observed: None,
                        response_timing: None,
                        observed_at: Some(observed_at),
                        request_fingerprint: "b".repeat(64),
                        http_status: Some(200),
                        usage: Some(ModelTransportUsage {
                            input_tokens: 30,
                            output_tokens: 9,
                            cache_read_tokens: Some(20),
                            cache_write_tokens: None,
                            cache_read_included_in_input: true,
                        }),
                        outcome: None,
                    },
                )
                .unwrap();
            store
                .record_model_step_transport(
                    &session,
                    "usage-turn",
                    "usage-step",
                    ModelTransportRecord {
                        usage_observed: None,
                        response_timing: Some(magi_app_server_protocol::ModelResponseTiming {
                            first_content_ms: 250,
                            content_duration_ms: 1000,
                            text_output_tokens: Some(2),
                        }),
                        observed_at: Some(observed_at),
                        request_fingerprint: "b".repeat(64),
                        http_status: Some(200),
                        usage: Some(ModelTransportUsage {
                            input_tokens: 20,
                            output_tokens: 2,
                            cache_read_tokens: (index == 1).then_some(10),
                            cache_write_tokens: None,
                            cache_read_included_in_input: true,
                        }),
                        outcome: Some(if index == 0 {
                            ModelTransportOutcome::Failed
                        } else {
                            ModelTransportOutcome::Succeeded
                        }),
                    },
                )
                .unwrap();
            if index == 0 {
                store
                    .start_model_step_retry(&session, "usage-turn", "usage-step")
                    .unwrap();
            }
        }
        if settled {
            store
                .settle_model_step(
                    &session,
                    "usage-turn",
                    "usage-step",
                    Some("c".repeat(64)),
                    None,
                    None,
                )
                .unwrap();
        }
        (store, session)
    }

    #[test]
    fn main_and_worker_retry_measurements_survive_restore_without_double_counting() {
        for role in ["orchestrator", "worker"] {
            let (store, session) = recorded_call_for_role(true, UtcMillis::now().0, role);
            let restored = SessionStore::from_persisted_parts(
                store.durable_state(),
                store.execution_sidecar_store_state(),
            )
            .unwrap();
            let bus = InMemoryEventBus::new(16);
            assert_eq!(recover_model_usage_projections(&bus, &restored).unwrap(), 2);
            assert_eq!(recover_model_usage_projections(&bus, &restored).unwrap(), 0);
            let mut authority = magi_usage_authority::UsageAuthority::new();
            for entry in bus.audit_usage_ledger_snapshot().usage_entries {
                let input: UsageCallRecordInput = serde_json::from_value(entry.payload).unwrap();
                assert_eq!(
                    serde_json::to_value(input.execution_binding.role).unwrap(),
                    role
                );
                assert_eq!(
                    serde_json::to_value(input.call_identity.source).unwrap(),
                    role
                );
                assert_eq!(input.session_id, session.as_str());
                authority.append_call_record(input.clone());
                authority.append_call_record(input);
            }
            let snapshot = authority.get_session_snapshot("personal", session.as_str());
            let totals = &snapshot.totals;
            assert_eq!(totals.llm_call_count, 2, "both physical attempts count");
            assert_eq!(totals.success_count, 1);
            assert_eq!(totals.failure_count, 1);
            assert_eq!(totals.token_observed_call_count, 2);
            assert_eq!(totals.raw_input_tokens, 40);
            assert_eq!(totals.net_output_tokens, 4);
            assert_eq!(totals.cache_observed_call_count, 1);
            assert_eq!(totals.cache_hit_call_count, 1);
            assert_eq!(totals.cache_observed_input_tokens, 20);
            assert_eq!(totals.cache_read_tokens, 10);
            assert_eq!(
                totals.performance.ttft_sample_count, 1,
                "failed attempts do not skew TTFT"
            );
            assert_eq!(totals.performance.ttft_total_ms, 250);
            assert_eq!(totals.performance.tps_sample_count, 1);
            assert_eq!(totals.performance.text_output_tokens, 2);
            assert_eq!(totals.performance.content_duration_ms, 1000);
            assert_eq!(totals.performance.last_tps, Some(2.0));
            assert_eq!(snapshot.by_execution_binding.len(), 1);
            assert_eq!(
                snapshot.by_execution_binding[0]
                    .totals
                    .performance
                    .ttft_total_ms,
                250
            );
        }
    }

    #[test]
    fn missing_projection_is_rebuilt_once_using_frozen_model_and_original_attempt_ids() {
        let (store, session) = recorded_call(true, UtcMillis::now().0);
        let restored = SessionStore::from_persisted_parts(
            store.durable_state(),
            store.execution_sidecar_store_state(),
        )
        .unwrap();
        let bus = InMemoryEventBus::new(16);
        assert_eq!(recover_model_usage_projections(&bus, &restored).unwrap(), 2);
        assert_eq!(recover_model_usage_projections(&bus, &restored).unwrap(), 0);
        let entries = bus.audit_usage_ledger_snapshot().usage_entries;
        assert_eq!(entries.len(), 2);
        for entry in &entries {
            let input: UsageCallRecordInput =
                serde_json::from_value(entry.payload.clone()).unwrap();
            assert_eq!(
                input
                    .call_identity
                    .transport
                    .as_ref()
                    .unwrap()
                    .response_timing
                    .as_ref()
                    .unwrap()
                    .first_content_ms,
                250
            );
            assert_eq!(input.model_config.model, "original-model");
            assert_eq!(input.model_config.base_url, "https://original.test/v1");
            assert_eq!(input.assignment_id.as_deref(), Some("original-lease"));
            assert_eq!(input.turn_id.as_deref(), Some("usage-turn"));
            assert_eq!(input.session_id, session.as_str());
            assert_eq!(
                input.call_identity.parent_call_id.as_deref(),
                Some("usage-step")
            );
        }
        // 第一次尝试已落盘、第二次尚未刷盘：恢复只补第二次，不重复计费。
        let mut partial = bus.audit_usage_ledger_snapshot();
        partial.usage_entries.truncate(1);
        let reloaded_bus = InMemoryEventBus::new(16);
        reloaded_bus.restore_persisted_audit_usage_ledger(partial);
        assert_eq!(
            recover_model_usage_projections(&reloaded_bus, &restored).unwrap(),
            1
        );
        assert_eq!(reloaded_bus.audit_usage_ledger_status().usage_count, 2);
    }

    #[test]
    fn crash_after_receipt_before_logical_settlement_preserves_unknown_outcome_and_known_usage() {
        let (store, session) = recorded_call(false, UtcMillis::now().0);
        let restored = SessionStore::from_persisted_parts(
            store.durable_state(),
            store.execution_sidecar_store_state(),
        )
        .unwrap();
        let bus = InMemoryEventBus::new(16);
        assert_eq!(recover_model_usage_projections(&bus, &restored).unwrap(), 2);
        let step = restored
            .canonical_turn_for_session_turn_id(&session, "usage-turn")
            .unwrap()
            .model_steps
            .remove(0);
        assert_eq!(step.status, ModelStepStatus::OutcomeUnknown);
        assert_eq!(step.usage_tokens, Some(44));
        assert!(step.response_fingerprint.is_none());
        assert_eq!(
            bus.audit_usage_ledger_snapshot()
                .usage_entries
                .last()
                .unwrap()
                .payload["status"],
            "failed"
        );
    }

    #[test]
    fn recovery_respects_stats_reset_and_does_not_promote_expired_receipts() {
        let observed = UtcMillis::now().0.saturating_sub(100);
        let (store, _) = recorded_call(false, observed);
        let bus = InMemoryEventBus::new(16);
        let mut reset = EventEnvelope::usage(
            EventId::new("reset"),
            magi_event_bus::USAGE_STATS_RESET_EVENT_TYPE,
            json!({}),
        );
        reset.occurred_at = UtcMillis(observed + 10);
        bus.publish(reset);
        assert_eq!(recover_model_usage_projections(&bus, &store).unwrap(), 0);
        assert_eq!(bus.audit_usage_ledger_status().usage_count, 1);
        let (expired, _) = recorded_call(
            true,
            UtcMillis::now()
                .0
                .saturating_sub(magi_event_bus::AUDIT_USAGE_LEDGER_RETENTION_MILLIS + 1000),
        );
        assert_eq!(
            recover_model_usage_projections(&InMemoryEventBus::new(8), &expired).unwrap(),
            0
        );
    }

    #[test]
    fn usage_scope_is_frozen_even_if_session_is_later_bound_to_another_workspace() {
        let (store, session) = recorded_call(true, UtcMillis::now().0);
        store.bind_execution_ownership(
            session.clone(),
            magi_core::ExecutionOwnership {
                session_id: Some(session.clone()),
                workspace_id: Some(WorkspaceId::new("later-workspace")),
                ..Default::default()
            },
        );
        let bus = InMemoryEventBus::new(8);
        assert_eq!(recover_model_usage_projections(&bus, &store).unwrap(), 2);
        for entry in bus.audit_usage_ledger_snapshot().usage_entries {
            assert!(entry.payload["workspaceId"].is_null());
            assert!(entry.event_id.starts_with("model-usage:personal:"));
        }
    }
}
