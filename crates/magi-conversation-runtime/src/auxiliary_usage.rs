//! 没有 Turn 的辅助请求在原用量账本记录传输事实；不创建虚构会话历史或第二个数据库。
use magi_app_server_protocol::{ModelUsageContext, ModelUsageSourceRole};
use magi_bridge_client::{BridgeClientError, BridgeErrorLayer};
use magi_core::{DomainError, DomainResult, EventId, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_session_store::ModelTransportRecord;
use magi_usage_authority::UsageCallStatus;
use serde::{Deserialize, Serialize};

const EVENT_TYPE: &str = "model.auxiliary.transport.observed";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AuxiliaryUsageFact {
    pub schema_version: u32,
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub call_id: String,
    pub attempt: usize,
    pub context: ModelUsageContext,
    pub transport: ModelTransportRecord,
    pub call_status: Option<UsageCallStatus>,
}

fn validate(fact: &AuxiliaryUsageFact) -> DomainResult<()> {
    magi_session_store::validate_model_usage_context(&fact.context)?;
    if fact.schema_version != 1
        || fact.attempt == 0
        || fact.call_id.trim().is_empty()
        || fact.session_id.as_str().trim().is_empty()
        || fact.context.role != ModelUsageSourceRole::Auxiliary
        || fact.transport.request_fingerprint.len() != 64
        || !fact
            .transport
            .request_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || fact.transport.observed_at.is_none()
        || fact
            .transport
            .http_status
            .is_some_and(|status| !(100..600).contains(&status))
    {
        return Err(DomainError::Validation {
            message: "辅助调用传输事实身份或版本无效".into(),
        });
    }
    Ok(())
}

pub(crate) fn write_fact(
    bus: &InMemoryEventBus,
    fact: &AuxiliaryUsageFact,
) -> Result<(), BridgeClientError> {
    let failed = |error: String| BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::RemoteBusiness,
        code: Some(-32096),
        message: format!("auxiliary_usage_journal_failed: {error}"),
    };
    validate(fact).map_err(|error| failed(error.to_string()))?;
    let state = match (
        &fact.transport.outcome,
        fact.transport.usage.is_some(),
        fact.transport.http_status,
    ) {
        (None, false, None) => "started".to_string(),
        (None, true, status) => format!("observed-{}", status.unwrap_or_default()),
        (None, false, status) => format!("observed-{}", status.unwrap_or_default()),
        (Some(outcome), _, status) => {
            format!("finished-{:?}-{}", outcome, status.unwrap_or_default())
        }
    };
    let mut event = EventEnvelope::usage(
        EventId::new(format!(
            "auxiliary-transport:{}:{}:{}:{}",
            fact.session_id, fact.call_id, fact.attempt, state
        )),
        EVENT_TYPE,
        serde_json::to_value(fact).map_err(|error| failed(error.to_string()))?,
    )
    .with_context(EventContext {
        session_id: Some(fact.session_id.clone()),
        workspace_id: fact
            .context
            .workspace_id
            .as_ref()
            .map(|id| WorkspaceId::new(id.clone())),
        ..Default::default()
    });
    event.occurred_at = UtcMillis(fact.transport.observed_at.expect("validated timestamp"));
    bus.publish_usage_projection(event);
    // 辅助调用没有 canonical Step 的事务日志；已收到的用量必须在返回/重试前持久化。
    bus.refresh_audit_usage_ledger_persistence()
        .map_err(|error| failed(error.to_string()))
}

pub(crate) fn publish_fact_usage(bus: &InMemoryEventBus, fact: &AuxiliaryUsageFact) {
    let attempt_id = format!("{}:attempt:{}", fact.call_id, fact.attempt);
    let record = crate::canonical_usage::transport_usage_record(
        crate::canonical_usage::TransportUsageInput {
            context: &fact.context,
            session_id: &fact.session_id,
            turn_id: fact.turn_id.as_deref(),
            logical_call_id: &fact.call_id,
            attempt_id: &attempt_id,
            transport: &fact.transport,
            confirmed_response: fact.call_status == Some(UsageCallStatus::Success),
            fallback_timestamp: fact.transport.observed_at.unwrap_or(0),
        },
    );
    crate::usage_recording::publish_usage_call_input(
        bus,
        &fact
            .context
            .workspace_id
            .as_ref()
            .map(|id| WorkspaceId::new(id.clone())),
        &fact.session_id,
        &record,
    );
}

pub(crate) fn recover(bus: &InMemoryEventBus) -> DomainResult<()> {
    let facts =
        bus.with_audit_usage_ledger(|ledger| {
            let mut latest =
                std::collections::BTreeMap::<(String, String, usize), AuxiliaryUsageFact>::new();
            for entry in ledger
                .usage_entries
                .iter()
                .filter(|entry| entry.event_type == EVENT_TYPE)
            {
                let fact: AuxiliaryUsageFact = serde_json::from_value(entry.payload.clone())
                    .map_err(|error| DomainError::Validation {
                        message: format!("辅助调用用量事实无法恢复: {error}"),
                    })?;
                validate(&fact)?;
                if let Some(previous) = latest.get(&(
                    fact.session_id.to_string(),
                    fact.call_id.clone(),
                    fact.attempt,
                )) {
                    if previous.context != fact.context
                        || previous.turn_id != fact.turn_id
                        || previous
                            .call_status
                            .is_some_and(|status| fact.call_status != Some(status))
                    {
                        return Err(DomainError::InvalidState {
                            message: "辅助调用的冻结统计归属或结果被改写".into(),
                        });
                    }
                    magi_session_store::validate_model_transport_update(
                        &previous.transport,
                        &fact.transport,
                    )?;
                }
                latest.insert(
                    (
                        fact.session_id.clone().to_string(),
                        fact.call_id.clone(),
                        fact.attempt,
                    ),
                    fact,
                );
            }
            Ok::<_, DomainError>(latest)
        })?;
    for fact in facts.values() {
        publish_fact_usage(bus, fact);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_event_bus::AuditUsageLedgerSnapshot;
    use magi_session_store::{ModelTransportOutcome, ModelTransportUsage, SessionStore};
    use serde_json::json;

    fn fact(at: u64) -> AuxiliaryUsageFact {
        AuxiliaryUsageFact {
            schema_version: 1, session_id: SessionId::new("aux-original-session"), turn_id: Some("original-turn".into()),
            call_id: "aux-call".into(), attempt: 1,
            context: serde_json::from_value(json!({
                "templateId":"auxiliary","engineId":"auxiliary","bindingRevision":1,"role":"auxiliary","phase":"integration",
                "workspaceId":null,"modelConfig":{"provider":"configured","model":"frozen-aux","baseUrl":"https://original.test/v1"}
            })).unwrap(),
            transport: ModelTransportRecord { usage_observed: None, response_timing: None, observed_at: Some(at), request_fingerprint: "d".repeat(64), http_status: Some(200),
                usage: Some(ModelTransportUsage { input_tokens: 12, output_tokens: 3, cache_read_tokens: None, cache_write_tokens: None, cache_read_included_in_input: false }),
                outcome: Some(ModelTransportOutcome::Succeeded) },
            call_status: None,
        }
    }

    #[test]
    fn durable_auxiliary_receipt_recovers_without_a_turn_or_a_second_provider_call() {
        let directory = std::env::temp_dir().join(format!(
            "magi-auxiliary-recovery-{}-{}",
            std::process::id(),
            UtcMillis::now().0
        ));
        let bus = InMemoryEventBus::new(16);
        bus.set_audit_usage_ledger_persistence(directory.clone());
        let mut source = fact(UtcMillis::now().0);
        write_fact(&bus, &source).unwrap();
        // HTTP 已完成、逻辑返回尚未交付。恢复不能把这个窗口猜成成功。
        let reloaded = InMemoryEventBus::new(16);
        reloaded.restore_persisted_audit_usage_ledger(
            AuditUsageLedgerSnapshot::load_from_dir(&directory, UtcMillis::now()).unwrap(),
        );
        reloaded.set_audit_usage_ledger_persistence(directory.clone());
        let store = SessionStore::new();
        assert_eq!(
            crate::canonical_usage::recover_model_usage_projections(&reloaded, &store).unwrap(),
            1
        );
        assert!(store.durable_state().canonical_turns.is_empty());
        let usage = reloaded
            .audit_usage_ledger_snapshot()
            .usage_entries
            .into_iter()
            .find(|entry| entry.event_type == "model.usage.recorded")
            .unwrap();
        assert_eq!(usage.payload["status"], "failed");
        assert_eq!(usage.payload["usage"]["inputTokens"], 12);
        assert!(usage.payload["usage"]["cacheReadTokens"].is_null());
        assert_eq!(usage.payload["modelConfig"]["model"], "frozen-aux");
        reloaded.refresh_audit_usage_ledger_persistence().unwrap();
        let again = InMemoryEventBus::new(16);
        again.restore_persisted_audit_usage_ledger(
            AuditUsageLedgerSnapshot::load_from_dir(&directory, UtcMillis::now()).unwrap(),
        );
        assert_eq!(
            crate::canonical_usage::recover_model_usage_projections(&again, &store).unwrap(),
            0
        );

        // 正常交付的另一调用保留成功事实，使用同一个投影器。
        source.call_id = "aux-confirmed".into();
        source.call_status = Some(UsageCallStatus::Success);
        write_fact(&again, &source).unwrap();
        recover(&again).unwrap();
        let confirmed = again
            .audit_usage_ledger_snapshot()
            .usage_entries
            .into_iter()
            .find(|entry| entry.event_id.ends_with("aux-confirmed:attempt:1"))
            .unwrap();
        assert_eq!(confirmed.payload["status"], "success");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn auxiliary_recovery_respects_reset_and_rejects_unsafe_binding() {
        let bus = InMemoryEventBus::new(16);
        let at = UtcMillis::now().0.saturating_sub(100);
        let mut source = fact(at);
        write_fact(&bus, &source).unwrap();
        let mut reset = EventEnvelope::usage(
            EventId::new("reset"),
            magi_event_bus::USAGE_STATS_RESET_EVENT_TYPE,
            json!({}),
        );
        reset.occurred_at = UtcMillis(at + 10);
        bus.publish(reset);
        recover(&bus).unwrap();
        assert!(
            bus.audit_usage_ledger_snapshot()
                .usage_entries
                .iter()
                .all(|entry| entry.event_type != "model.usage.recorded")
        );
        source.context.model_config.base_url =
            "https://user:password@original.test/v1?key=secret".into();
        assert!(write_fact(&bus, &source).is_err());
        assert_eq!(bus.audit_usage_ledger_status().usage_count, 2);
    }

    #[test]
    fn auxiliary_journal_failure_blocks_the_http_send() {
        use crate::usage_recording::{
            AuxiliaryModelPurpose, AuxiliaryModelUsageContext, invoke_auxiliary_model_with_usage,
        };
        use magi_bridge_client::{HttpModelBridgeClient, ModelInvocationRequest};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let file = std::env::temp_dir().join(format!(
            "magi-auxiliary-ledger-file-{}-{}",
            std::process::id(),
            UtcMillis::now().0
        ));
        std::fs::write(&file, "not a ledger directory").unwrap();
        let bus = InMemoryEventBus::new(8);
        bus.set_audit_usage_ledger_persistence(file.clone());
        let settings = std::sync::Arc::new(magi_settings_store::SettingsStore::new());
        settings.set_section("auxiliary", json!({"baseUrl":url,"apiProtocol":"openai_chat","apiKey":"private-key","model":"test"})).unwrap();
        let store = SessionStore::new();
        let session = SessionId::new("auxiliary-journal-failure");
        let error = invoke_auxiliary_model_with_usage(
            std::sync::Arc::new(HttpModelBridgeClient::new(url, None, "test".into())),
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "configured".into(),
                prompt: "work".into(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            AuxiliaryModelUsageContext {
                event_bus: &bus,
                session_store: &store,
                settings_store: Some(&settings),
                session_id: Some(&session),
                workspace_id: &None,
                call_id: "journal-failure".into(),
                phase: magi_usage_authority::UsagePhase::Integration,
                purpose: AuxiliaryModelPurpose::SessionTitle,
                expected_turn_id: None,
            },
        )
        .unwrap_err();
        assert_eq!(error.code(), Some(-32096));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(
            !serde_json::to_string(&bus.audit_usage_ledger_snapshot())
                .unwrap()
                .contains("private-key")
        );
        std::fs::remove_file(file).unwrap();
    }
}
