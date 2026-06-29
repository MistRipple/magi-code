//! 任务系统 — usage recording helpers。
//!
//! 复用内部的 `model_config::NormalizedModelConfig` 与独立设置域。

use crate::model_config::{NormalizedModelConfig, configured_role_engine_model_config};
use magi_core::{EventId, MissionId, MissionLifecyclePhase, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_mission_metrics::{MissionMetricsStore, TurnUsage};
use magi_orchestrator::task_worker_catalog::WorkerInfo;
use magi_session_store::SessionStore;
use magi_settings_store::SettingsStore;
use magi_usage_authority::{
    ExecutionBindingIdentity, LlmConfig, UsageCallIdentity, UsageCallRecordInput, UsageCallStatus,
    UsagePhase, UsageSourceRole, UsageTokenInput,
};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct ModelUsageBinding {
    template_id: String,
    engine_id: String,
    binding_revision: u32,
    role: UsageSourceRole,
    phase: UsagePhase,
}

pub fn session_turn_model_usage_binding(use_tools: bool) -> ModelUsageBinding {
    ModelUsageBinding {
        template_id: "orchestrator".to_string(),
        engine_id: "orchestrator".to_string(),
        binding_revision: 0,
        role: UsageSourceRole::Orchestrator,
        phase: if use_tools {
            UsagePhase::Execution
        } else {
            UsagePhase::Planning
        },
    }
}

pub fn model_usage_binding_for_worker(worker: &WorkerInfo, is_primary: bool) -> ModelUsageBinding {
    if is_primary {
        return ModelUsageBinding {
            template_id: "orchestrator".to_string(),
            engine_id: "orchestrator".to_string(),
            binding_revision: 0,
            role: UsageSourceRole::Orchestrator,
            phase: UsagePhase::Planning,
        };
    }
    ModelUsageBinding {
        template_id: worker.role.clone(),
        engine_id: worker.worker_id.to_string(),
        binding_revision: 0,
        role: UsageSourceRole::Worker,
        phase: UsagePhase::Execution,
    }
}

pub fn model_usage_binding_for_worker_with_settings(
    worker: &WorkerInfo,
    is_primary: bool,
    settings_store: Option<&Arc<SettingsStore>>,
) -> ModelUsageBinding {
    let mut binding = model_usage_binding_for_worker(worker, is_primary);
    if is_primary {
        return binding;
    }
    if let Some(store) = settings_store
        && let Ok(Some(role_model)) = configured_role_engine_model_config(store, &worker.role)
    {
        binding.engine_id = role_model.engine_id;
        binding.binding_revision = role_model.binding_revision;
    }
    binding
}

pub fn publish_model_usage_record(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    settings_store: Option<&Arc<SettingsStore>>,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    binding: &ModelUsageBinding,
    call_id: String,
    usage: Option<&serde_json::Value>,
    status: UsageCallStatus,
    assignment_id: Option<String>,
    error_code: Option<String>,
) {
    let Some(usage) = usage_tokens_from_payload(usage) else {
        return;
    };
    let Some(model_config) = usage_model_config_for_binding(settings_store, binding) else {
        tracing::warn!(
            template_id = binding.template_id,
            engine_id = binding.engine_id,
            "模型调用已返回用量，但缺少可审计的模型配置，跳过统计记录"
        );
        return;
    };
    let Some(workspace_id) = workspace_id.as_ref() else {
        tracing::warn!(
            session_id = %session_id,
            call_id = %call_id,
            "模型调用已返回用量，但缺少 workspace 绑定，跳过统计记录"
        );
        return;
    };
    let workspace_id_value = workspace_id.to_string();
    let input = UsageCallRecordInput {
        workspace_id: workspace_id_value.clone(),
        session_id: session_id.to_string(),
        turn_id: current_turn_id(session_store, session_id),
        dispatch_wave_id: None,
        assignment_id,
        event_id: Some(format!(
            "model-usage:{}:{}:{}",
            workspace_id_value, session_id, call_id
        )),
        timestamp: Some(UtcMillis::now().0),
        execution_binding: ExecutionBindingIdentity {
            template_id: binding.template_id.clone(),
            engine_id: binding.engine_id.clone(),
            binding_revision: binding.binding_revision,
            role: binding.role,
        },
        model_config,
        call_identity: UsageCallIdentity {
            call_id,
            parent_call_id: None,
            source: binding.role,
            phase: binding.phase,
        },
        usage,
        status,
        error_code,
    };
    let payload = match serde_json::to_value(&input) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(?error, "序列化模型用量记录失败");
            return;
        }
    };
    let _ = event_bus.publish(
        EventEnvelope::usage(
            EventId::new(
                input
                    .event_id
                    .clone()
                    .unwrap_or_else(|| format!("model-usage-{}", UtcMillis::now().0)),
            ),
            "model.usage.recorded",
            payload,
        )
        .with_context(EventContext {
            workspace_id: Some(workspace_id.clone()),
            session_id: Some(session_id.clone()),
            assignment_id: input
                .assignment_id
                .clone()
                .map(magi_core::AssignmentId::new),
            ..EventContext::default()
        }),
    );
}

fn current_turn_id(session_store: &SessionStore, session_id: &SessionId) -> Option<String> {
    session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .map(|turn| turn.turn_id)
}

/// Mission 维度记账（与 `publish_model_usage_record` 并列的单一写点）。
///
/// 在 conversation_loop 中每一轮 LLM 调用结束后调用一次，把本轮 token / 时间窗口
/// 累加到 mission-scoped `metrics.md` sidecar。设计上：
/// - 与 `publish_model_usage_record` 共享 `usage_tokens_from_payload`，保证 token 口径一致；
/// - 写入失败仅 `warn!`，不阻断主轮次（accounting 出错绝不影响对话）；
/// - phase 由调用方按需传入，缺省 `None` 表示尚未判定。
pub fn record_mission_turn(
    store: &MissionMetricsStore,
    mission_id: &MissionId,
    usage: Option<&serde_json::Value>,
    started_at: UtcMillis,
    finished_at: UtcMillis,
    phase: Option<MissionLifecyclePhase>,
) {
    let Some(tokens) = usage_tokens_from_payload(usage) else {
        return;
    };
    let turn_usage = TurnUsage {
        prompt_tokens: tokens.input_tokens,
        completion_tokens: tokens.output_tokens,
        started_at,
        finished_at,
        phase,
    };
    if let Err(error) = store.record_turn(mission_id, turn_usage) {
        tracing::warn!(?error, mission_id = %mission_id, "mission metrics 写入失败，已跳过本轮记账");
    }
}

fn usage_model_config_for_binding(
    settings_store: Option<&Arc<SettingsStore>>,
    binding: &ModelUsageBinding,
) -> Option<LlmConfig> {
    let store = settings_store?;
    if matches!(binding.role, UsageSourceRole::Worker) {
        if let Ok(Some(role_model)) =
            configured_role_engine_model_config(store, &binding.template_id)
        {
            return role_model.config.to_usage_llm_config();
        }
        let workers = store.get_section("workers");
        if let Some(config) = workers
            .get(&binding.engine_id)
            .or_else(|| workers.get(&binding.template_id))
            .and_then(
                |value| match NormalizedModelConfig::from_settings_value(value) {
                    Ok(config) => Some(config),
                    Err(error) => {
                        tracing::warn!(
                            role = %binding.template_id,
                            engine = %binding.engine_id,
                            error = %error,
                            "worker 模型配置无效，跳过用量身份归因"
                        );
                        None
                    }
                },
            )
            .and_then(|config| config.to_usage_llm_config())
        {
            return Some(config);
        }
    }
    let orchestrator = store.get_section("orchestrator");
    match NormalizedModelConfig::from_settings_value(&orchestrator) {
        Ok(config) => config.to_usage_llm_config(),
        Err(error) => {
            tracing::warn!(error = %error, "orchestrator 模型配置无效，跳过用量身份归因");
            None
        }
    }
}

fn usage_tokens_from_payload(usage: Option<&serde_json::Value>) -> Option<UsageTokenInput> {
    let usage = usage?;
    let input_tokens = usage_u64_field(
        usage,
        &[
            "prompt_tokens",
            "input_tokens",
            "promptTokens",
            "inputTokens",
        ],
    )
    .unwrap_or(0);
    let output_tokens = usage_u64_field(
        usage,
        &[
            "completion_tokens",
            "output_tokens",
            "completionTokens",
            "outputTokens",
        ],
    )
    .unwrap_or(0);
    let total_tokens = usage_u64_field(usage, &["total_tokens", "totalTokens"]).unwrap_or(0);
    let input_tokens = if input_tokens == 0 && output_tokens == 0 {
        total_tokens
    } else {
        input_tokens
    };
    if input_tokens == 0 && output_tokens == 0 {
        return None;
    }
    let included_cache_read_tokens = usage_u64_pointer(
        usage,
        &[
            "/prompt_tokens_details/cached_tokens",
            "/input_tokens_details/cached_tokens",
            "/promptTokensDetails/cachedTokens",
            "/inputTokensDetails/cachedTokens",
        ],
    );
    let explicit_cache_read_tokens = usage_u64_field(
        usage,
        &[
            "cache_read_input_tokens",
            "cacheReadInputTokens",
            "cache_read_tokens",
            "cacheReadTokens",
        ],
    );
    let cache_read_included_in_input = usage_bool_field(
        usage,
        &["cache_read_included_in_input", "cacheReadIncludedInInput"],
    )
    .unwrap_or(included_cache_read_tokens.is_some());
    Some(UsageTokenInput {
        input_tokens,
        output_tokens,
        total_tokens: (total_tokens > 0).then_some(total_tokens),
        cache_read_tokens: included_cache_read_tokens.or(explicit_cache_read_tokens),
        cache_write_tokens: usage_u64_field(
            usage,
            &[
                "cache_creation_input_tokens",
                "cacheCreationInputTokens",
                "cache_write_tokens",
                "cacheWriteTokens",
            ],
        ),
        cache_read_included_in_input,
    })
}

fn usage_u64_field(usage: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| usage.get(*key).and_then(|value| value.as_u64()))
}

fn usage_u64_pointer(usage: &serde_json::Value, pointers: &[&str]) -> Option<u64> {
    pointers
        .iter()
        .find_map(|pointer| usage.pointer(pointer).and_then(|value| value.as_u64()))
}

fn usage_bool_field(usage: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| usage.get(*key).and_then(|value| value.as_bool()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_event_bus::InMemoryEventBus;
    use magi_session_store::SessionStore;
    use magi_usage_authority::UsageCallStatus;
    use serde_json::json;

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
        let session_store = SessionStore::new();
        let settings_store = Arc::new(SettingsStore::new());
        settings_store.set_section(
            "orchestrator",
            json!({
                "baseUrl": "https://example.test",
                "model": "gpt-test",
                "provider": "openai-compatible"
            }),
        );
        let binding = session_turn_model_usage_binding(true);
        let workspace_id = Some(WorkspaceId::new("workspace-1"));

        publish_model_usage_record(
            &event_bus,
            &session_store,
            Some(&settings_store),
            &SessionId::new("session-1"),
            &workspace_id,
            &binding,
            "call-1".to_string(),
            Some(&json!({"prompt_tokens": 3, "completion_tokens": 7})),
            UsageCallStatus::Success,
            Some("assignment-1".to_string()),
            None,
        );

        let snapshot = event_bus.snapshot();
        assert_eq!(snapshot.recent_events.len(), 1);
        assert_eq!(snapshot.recent_events[0].event_type, "model.usage.recorded");
        assert_eq!(
            snapshot.recent_events[0].category,
            magi_event_bus::EventCategory::Usage
        );
        assert_eq!(
            snapshot.recent_events[0].workspace_id.as_ref(),
            workspace_id.as_ref()
        );
        assert_eq!(
            snapshot.recent_events[0].payload["workspaceId"],
            json!("workspace-1")
        );
    }

    #[test]
    fn publish_model_usage_record_skips_usage_event_without_workspace() {
        let event_bus = InMemoryEventBus::new(8);
        let session_store = SessionStore::new();
        let settings_store = Arc::new(SettingsStore::new());
        settings_store.set_section(
            "orchestrator",
            json!({
                "baseUrl": "https://example.test",
                "model": "gpt-test",
                "provider": "openai-compatible"
            }),
        );
        let binding = session_turn_model_usage_binding(true);

        publish_model_usage_record(
            &event_bus,
            &session_store,
            Some(&settings_store),
            &SessionId::new("session-1"),
            &None,
            &binding,
            "call-1".to_string(),
            Some(&json!({"prompt_tokens": 3, "completion_tokens": 7})),
            UsageCallStatus::Success,
            Some("assignment-1".to_string()),
            None,
        );

        assert!(
            event_bus.snapshot().recent_events.is_empty(),
            "workspace-less model usage must not be written into a synthetic workspace"
        );
    }
}
