use crate::{
    model_config::NormalizedModelConfig,
    prompt_utils::{PromptFragmentKind, render_prompt_fragment},
    tool_result_utils::infer_tool_call_status,
    usage_recording::{
        ModelUsageRecordInput, auxiliary_model_usage_binding, publish_model_usage_record,
    },
};
use magi_bridge_client::{
    ChatMessage, ChatToolDefinition, ModelBridgeClient, ModelInvocationRequest,
};
use magi_core::{EventId, SessionId, ThreadId, UtcMillis, WorkspaceId, estimate_text_tokens};
use magi_event_bus::{
    EventContext, EventEnvelope, InMemoryEventBus, SessionRuntimeUsageObservation,
    latest_usage_observations_from_ledger,
};
use magi_session_store::{
    SessionStore, ThreadChatMessage, ThreadContextCheckpoint, ThreadFileFactVersion,
};
use magi_settings_store::SettingsStore;
use magi_usage_authority::{
    AUTO_COMPACT_PERCENT, DEFAULT_CONTEXT_WINDOW, UsageCallStatus, UsagePhase,
    resolve_context_window,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};

const THREAD_HISTORY_COMPACT_TARGET_TOKENS: usize = 8_000;
const THREAD_HISTORY_ESTIMATED_PREFILL_COMPACT_PERCENT: i64 = 90;
const THREAD_HISTORY_RECOVERY_TARGET_PERCENT: u64 = 70;
const THREAD_HISTORY_RECENT_MESSAGE_TARGET: usize = 12;
const COMPACTION_INPUT_WINDOW_PERCENT: u64 = 60;
const COMPACTION_MAX_SOURCE_TOKENS: usize = 32_000;
const COMPACTION_PROMPT_RESERVE_TOKENS: usize = 768;
const COMPACTION_MAX_REDUCTION_LEVELS: usize = 8;

#[derive(Clone, Debug)]
pub(crate) struct ContextCompactionRecord {
    pub reason: &'static str,
    pub original_message_count: usize,
    pub compacted_message_count: usize,
    pub original_token_estimate: usize,
    pub compacted_token_estimate: usize,
    pub compacted_at: UtcMillis,
}

pub(crate) struct PreparedThreadHistory {
    pub messages: Vec<ThreadChatMessage>,
    pub compaction: Option<ContextCompactionRecord>,
}

pub(crate) struct ContextAuthority<'a> {
    client: &'a dyn ModelBridgeClient,
    event_bus: &'a InMemoryEventBus,
    session_store: &'a SessionStore,
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
    thread_id: &'a ThreadId,
    settings_store: Option<&'a Arc<SettingsStore>>,
}

pub(crate) struct ContextPrepareRequest {
    pub fallback_history: Vec<ThreadChatMessage>,
    pub phase: &'static str,
    pub context_window_override: Option<u64>,
    pub additional_token_estimate: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct CurrentFileFact {
    pub absolute_path: String,
    pub content_hash: String,
    pub result: String,
    pub summary: String,
}

#[derive(Clone, Debug)]
pub(crate) enum ThreadHistoryCompactionDecision {
    ContextWindowPressure {
        tokens_used: u64,
        token_limit: u64,
        threshold_tokens: u64,
        target_history_tokens: usize,
        resolved_model: Option<String>,
    },
    EstimatedPrefill {
        estimated_tokens: usize,
        threshold_tokens: usize,
        target_history_tokens: usize,
    },
}

impl ThreadHistoryCompactionDecision {
    fn reason_label(&self) -> &'static str {
        match self {
            Self::ContextWindowPressure { .. } => "context_window_pressure",
            Self::EstimatedPrefill { .. } => "estimated_prefill",
        }
    }

    fn target_history_tokens(&self) -> usize {
        match self {
            Self::ContextWindowPressure {
                target_history_tokens,
                ..
            }
            | Self::EstimatedPrefill {
                target_history_tokens,
                ..
            } => *target_history_tokens,
        }
    }

    fn context_window_tokens(&self) -> u64 {
        match self {
            Self::ContextWindowPressure { token_limit, .. } => *token_limit,
            Self::EstimatedPrefill { .. } => DEFAULT_CONTEXT_WINDOW.max(0) as u64,
        }
    }
}

impl<'a> ContextAuthority<'a> {
    pub(crate) fn new(
        client: &'a dyn ModelBridgeClient,
        event_bus: &'a InMemoryEventBus,
        session_store: &'a SessionStore,
        session_id: &'a SessionId,
        workspace_id: &'a Option<WorkspaceId>,
        thread_id: &'a ThreadId,
        settings_store: Option<&'a Arc<SettingsStore>>,
    ) -> Self {
        Self {
            client,
            event_bus,
            session_store,
            session_id,
            workspace_id,
            thread_id,
            settings_store,
        }
    }

    pub(crate) fn prepare(&self, request: ContextPrepareRequest) -> PreparedThreadHistory {
        if let Some(context_window) = request.context_window_override {
            self.session_store.record_thread_context_window_tokens(
                self.thread_id,
                context_window,
                UtcMillis::now(),
            );
        }
        let effective_context_window = request.context_window_override.or_else(|| {
            self.session_store
                .thread_context_window_tokens(self.thread_id)
        });
        let mut transcript = self.session_store.thread_message_history(self.thread_id);
        if transcript.is_empty() && !request.fallback_history.is_empty() {
            transcript = request.fallback_history;
            self.session_store.replace_thread_messages(
                self.thread_id,
                transcript.clone(),
                UtcMillis::now(),
            );
        }
        if let Some(checkpoint) = self.session_store.thread_context_checkpoint(self.thread_id)
            && !checkpoint_file_facts_are_current(&checkpoint)
        {
            self.session_store
                .clear_thread_context_checkpoint(self.thread_id);
            tracing::info!(
                thread_id = %self.thread_id,
                session_id = %self.session_id,
                checkpoint_id = checkpoint.checkpoint_id,
                "上下文检查点因文件事实版本变化失效，将从原始 transcript 重建"
            );
        }
        let previous_checkpoint = self.session_store.thread_context_checkpoint(self.thread_id);
        let mut history = self.session_store.thread_context_history(self.thread_id);
        validate_workspace_file_facts(&mut history);
        let original_count = history.len();
        let original_tokens = estimate_thread_history_tokens(&history);
        let usage_observation = latest_session_usage_observation(self.event_bus, self.session_id)
            .filter(|observation| {
                previous_checkpoint.as_ref().is_none_or(|checkpoint| {
                    observation
                        .observed_at
                        .is_none_or(|observed_at| observed_at.0 > checkpoint.created_at.0)
                })
            });
        let Some(decision) = thread_history_compaction_decision(
            &history,
            usage_observation.as_ref(),
            effective_context_window,
            request.additional_token_estimate,
        ) else {
            tracing::debug!(
                thread_id = %self.thread_id,
                session_id = %self.session_id,
                phase = request.phase,
                original_count,
                original_tokens,
                last_context_window_tokens = usage_observation
                    .as_ref()
                    .map(|observation| observation.context_window_tokens),
                "thread 历史未达到上下文压缩阈值"
            );
            return PreparedThreadHistory {
                messages: history,
                compaction: None,
            };
        };

        let (compacted, split) = match self.compact_if_needed(&history, &decision) {
            Ok(Some(result)) => result,
            Ok(None) => {
                return PreparedThreadHistory {
                    messages: history,
                    compaction: None,
                };
            }
            Err(error) => {
                tracing::warn!(
                    thread_id = %self.thread_id,
                    session_id = %self.session_id,
                    phase = request.phase,
                    %error,
                    "上下文语义压缩失败，保留完整上下文"
                );
                return PreparedThreadHistory {
                    messages: history,
                    compaction: None,
                };
            }
        };

        let compacted_count = compacted.len();
        let compacted_tokens = estimate_thread_history_tokens(&compacted);
        let request_tokens = compacted_tokens.saturating_add(request.additional_token_estimate);
        let compacted_at = UtcMillis::now();
        let previous_source_count = previous_checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.source_message_count)
            .unwrap_or_default();
        let source_message_count = if previous_source_count == 0 {
            split
        } else {
            previous_source_count.saturating_add(split.saturating_sub(1))
        };
        self.session_store.install_thread_context_checkpoint(
            self.thread_id,
            ThreadContextCheckpoint {
                thread_id: self.thread_id.clone(),
                checkpoint_id: format!("context-checkpoint-{}", compacted_at.0),
                source_message_count,
                summary_message: compacted[0].clone(),
                reason: decision.reason_label().to_string(),
                original_token_estimate: original_tokens,
                checkpoint_token_estimate: compacted_tokens,
                created_at: compacted_at,
                file_fact_versions: collect_file_fact_versions(
                    &history[..split],
                    previous_checkpoint.as_ref(),
                ),
            },
            compacted_at,
        );
        self.publish_compaction(
            request.phase,
            &decision,
            original_count,
            compacted_count,
            original_tokens,
            compacted_tokens,
            request_tokens,
            compacted_at,
        );

        PreparedThreadHistory {
            messages: compacted,
            compaction: Some(ContextCompactionRecord {
                reason: decision.reason_label(),
                original_message_count: original_count,
                compacted_message_count: compacted_count,
                original_token_estimate: original_tokens,
                compacted_token_estimate: compacted_tokens,
                compacted_at,
            }),
        }
    }

    fn compact_if_needed(
        &self,
        history: &[ThreadChatMessage],
        decision: &ThreadHistoryCompactionDecision,
    ) -> Result<Option<(Vec<ThreadChatMessage>, usize)>, String> {
        let original_tokens = estimate_thread_history_tokens(history);
        let target_history_tokens = decision.target_history_tokens();
        if original_tokens <= target_history_tokens {
            tracing::debug!(
                reason = decision.reason_label(),
                original_tokens,
                target_tokens = target_history_tokens,
                "thread 历史已低于压缩目标，跳过重复压缩"
            );
            return Ok(None);
        }
        let summary_target_tokens = target_history_tokens.div_ceil(3).clamp(256, 2_000);
        let tail_target_tokens = target_history_tokens.saturating_sub(summary_target_tokens);
        let Some(split) = choose_thread_history_compaction_split(history, tail_target_tokens)
        else {
            return Ok(None);
        };
        let summary = self.build_compaction_message(
            &history[..split],
            original_tokens,
            decision.context_window_tokens(),
            summary_target_tokens,
        )?;
        let mut compacted = Vec::with_capacity(history.len().saturating_sub(split) + 1);
        compacted.push(summary);
        compacted.extend(history[split..].iter().cloned());
        if estimate_thread_history_tokens(&compacted) >= original_tokens {
            return Ok(None);
        }
        Ok(Some((compacted, split)))
    }

    fn build_compaction_message(
        &self,
        compacted_prefix: &[ThreadChatMessage],
        original_tokens: usize,
        context_window_tokens: u64,
        summary_target_tokens: usize,
    ) -> Result<ThreadChatMessage, String> {
        let auxiliary_client = self
            .settings_store
            .map(|store| store.get_section("auxiliary"))
            .map(|config| NormalizedModelConfig::from_settings_value(&config))
            .transpose()?
            .and_then(|config| config.to_http_model_client());
        let compaction_client: &dyn ModelBridgeClient = auxiliary_client
            .as_ref()
            .map(|client| client as &dyn ModelBridgeClient)
            .unwrap_or(self.client);
        let source_budget =
            (context_window_tokens.saturating_mul(COMPACTION_INPUT_WINDOW_PERCENT) / 100) as usize;
        let source_budget = source_budget
            .saturating_sub(COMPACTION_PROMPT_RESERVE_TOKENS)
            .clamp(256, COMPACTION_MAX_SOURCE_TOKENS);
        let serialized_messages = compacted_prefix
            .iter()
            .enumerate()
            .map(|(index, message)| {
                serde_json::to_string(message)
                    .map(|serialized| format!("message_index={index}\n{serialized}"))
                    .map_err(|error| format!("序列化待压缩上下文失败：{error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let source_chunks = pack_compaction_sources(serialized_messages, source_budget);
        let mut summaries = Vec::with_capacity(source_chunks.len());
        for (index, chunk) in source_chunks.iter().enumerate() {
            summaries.push(self.invoke_compaction_summary(
                compaction_client,
                chunk,
                "history_chunk",
                index,
                source_chunks.len(),
                summary_target_tokens,
            )?);
        }
        let mut reduction_level = 0usize;
        while summaries.len() > 1
            || summaries
                .first()
                .is_some_and(|summary| estimate_text_tokens(summary) > summary_target_tokens)
        {
            if reduction_level >= COMPACTION_MAX_REDUCTION_LEVELS {
                return Err("上下文压缩摘要未能在有限层级内收敛".to_string());
            }
            let before_tokens = summaries
                .iter()
                .map(|summary| estimate_text_tokens(summary))
                .sum::<usize>();
            let chunks = pack_compaction_sources(summaries, source_budget);
            let mut reduced = Vec::with_capacity(chunks.len());
            for (index, chunk) in chunks.iter().enumerate() {
                reduced.push(self.invoke_compaction_summary(
                    compaction_client,
                    chunk,
                    "summary_merge",
                    index,
                    chunks.len(),
                    summary_target_tokens,
                )?);
            }
            let after_tokens = reduced
                .iter()
                .map(|summary| estimate_text_tokens(summary))
                .sum::<usize>();
            if after_tokens >= before_tokens {
                return Err(format!(
                    "上下文压缩摘要未缩小：压缩前 {before_tokens} token，压缩后 {after_tokens} token"
                ));
            }
            summaries = reduced;
            reduction_level = reduction_level.saturating_add(1);
        }
        let summary = summaries
            .pop()
            .filter(|summary| !summary.trim().is_empty())
            .ok_or_else(|| "上下文压缩模型未返回摘要".to_string())?;
        let content = render_prompt_fragment(
            PromptFragmentKind::ThreadHistoryBoundary,
            format!(
                "[context_compaction]\n\
这是 Magi 自动生成的当前 thread 早期历史摘要，用于构建模型上下文视图。它是历史事实；如果它与后续保留的完整消息冲突，以后续完整消息为准。\n\
压缩范围：{} 条消息；压缩前估算 token：{}。\n\
语义交接摘要：\n{}\n\
[/context_compaction]",
                compacted_prefix.len(),
                original_tokens,
                summary,
            ),
        );
        Ok(ThreadChatMessage {
            role: "system".to_string(),
            content: Some(content),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn invoke_compaction_summary(
        &self,
        client: &dyn ModelBridgeClient,
        source: &str,
        stage: &str,
        chunk_index: usize,
        chunk_count: usize,
        summary_target_tokens: usize,
    ) -> Result<String, String> {
        let prompt = format!(
            "你是 Magi 的上下文压缩器。请把下面的历史片段转换为供后续模型继续工作的语义交接摘要。\n\
必须保留：用户目标与约束、已确认事实、文件路径和符号、工具调用结果、错误与未解决问题、计划进度、已经完成的外部操作。\n\
禁止：臆测、遗漏关键标识、把失败写成成功、给出面向用户的寒暄。\n\
使用与历史主要语言一致的 Markdown，按‘目标与约束 / 已完成 / 关键事实 / 未完成与下一步’组织。\n\
这是 {stage} 阶段的第 {}/{} 个片段；摘要必须压缩到约 {} token 以内。\n\
待压缩内容：\n{}",
            chunk_index + 1,
            chunk_count,
            summary_target_tokens,
            source,
        );
        let request = ModelInvocationRequest {
            provider: "context-compaction".to_string(),
            prompt: prompt.clone(),
            messages: Some(vec![ChatMessage {
                role: "user".to_string(),
                content: Some(prompt),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            }]),
            tools: None,
            tool_choice: None,
        };
        let call_id = format!(
            "context-compaction-{}-{}-{}-{}",
            self.session_id,
            UtcMillis::now().0,
            stage,
            chunk_index,
        );
        let response = client.invoke(request);
        let binding = auxiliary_model_usage_binding(UsagePhase::Integration);
        publish_model_usage_record(
            self.event_bus,
            self.session_store,
            self.settings_store,
            ModelUsageRecordInput {
                session_id: self.session_id,
                workspace_id: self.workspace_id,
                binding: &binding,
                call_id,
                usage: response
                    .as_ref()
                    .ok()
                    .and_then(|response| response.usage.as_ref()),
                status: if response.is_ok() {
                    UsageCallStatus::Success
                } else {
                    UsageCallStatus::Failed
                },
                assignment_id: None,
                error_code: response
                    .is_err()
                    .then(|| "context_compaction_failed".to_string()),
            },
        );
        response
            .map_err(|error| format!("上下文压缩模型调用失败：{error}"))?
            .content
            .map(|content| content.trim().to_string())
            .filter(|content| !content.is_empty())
            .ok_or_else(|| "上下文压缩模型未返回摘要".to_string())
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_compaction(
        &self,
        phase: &'static str,
        decision: &ThreadHistoryCompactionDecision,
        original_count: usize,
        compacted_count: usize,
        original_tokens: usize,
        compacted_tokens: usize,
        request_tokens: usize,
        compacted_at: UtcMillis,
    ) {
        let thread_scope = self
            .session_store
            .orchestrator_thread_for_session(self.session_id)
            .filter(|thread| thread.thread_id == *self.thread_id)
            .map(|_| "mainline")
            .unwrap_or("worker");
        let mut payload = serde_json::json!({
            "title": "上下文已自动压缩",
            "thread_id": self.thread_id.to_string(),
            "session_id": self.session_id.to_string(),
            "phase": phase,
            "reason": decision.reason_label(),
            "original_message_count": original_count,
            "compacted_message_count": compacted_count,
            "original_token_estimate": original_tokens,
            "compacted_token_estimate": compacted_tokens,
            "request_token_estimate": request_tokens,
            "compacted_at": compacted_at.0,
            "thread_scope": thread_scope,
        });
        match decision {
            ThreadHistoryCompactionDecision::ContextWindowPressure {
                tokens_used,
                token_limit,
                threshold_tokens,
                target_history_tokens,
                resolved_model,
            } => {
                if let Some(object) = payload.as_object_mut() {
                    object.insert("context_window_tokens".to_string(), (*tokens_used).into());
                    object.insert("token_limit".to_string(), (*token_limit).into());
                    object.insert("threshold_tokens".to_string(), (*threshold_tokens).into());
                    object.insert(
                        "target_history_tokens".to_string(),
                        (*target_history_tokens as u64).into(),
                    );
                    object.insert(
                        "resolved_model".to_string(),
                        resolved_model.as_deref().unwrap_or("").into(),
                    );
                }
                tracing::info!(
                    thread_id = %self.thread_id,
                    session_id = %self.session_id,
                    phase,
                    reason = decision.reason_label(),
                    original_count,
                    compacted_count,
                    original_tokens,
                    compacted_tokens,
                    tokens_used,
                    token_limit,
                    threshold_tokens,
                    target_history_tokens,
                    resolved_model = resolved_model.as_deref().unwrap_or(""),
                    "thread 模型上下文视图已按窗口压力生成新检查点"
                );
            }
            ThreadHistoryCompactionDecision::EstimatedPrefill {
                estimated_tokens,
                threshold_tokens,
                target_history_tokens,
            } => {
                if let Some(object) = payload.as_object_mut() {
                    object.insert(
                        "context_window_tokens".to_string(),
                        (*estimated_tokens as u64).into(),
                    );
                    object.insert(
                        "token_limit".to_string(),
                        (DEFAULT_CONTEXT_WINDOW.max(0) as u64).into(),
                    );
                    object.insert(
                        "threshold_tokens".to_string(),
                        (*threshold_tokens as u64).into(),
                    );
                    object.insert(
                        "target_history_tokens".to_string(),
                        (*target_history_tokens as u64).into(),
                    );
                }
                tracing::info!(
                    thread_id = %self.thread_id,
                    session_id = %self.session_id,
                    phase,
                    reason = decision.reason_label(),
                    original_count,
                    compacted_count,
                    original_tokens,
                    compacted_tokens,
                    estimated_tokens,
                    threshold_tokens,
                    target_history_tokens,
                    "thread 模型上下文视图已按冷启动估算压力生成新检查点"
                );
            }
        }
        let _ = self.event_bus.publish(
            EventEnvelope::usage(
                EventId::new(format!(
                    "event-session-context-compacted-{}",
                    compacted_at.0
                )),
                "session.context.compacted",
                payload,
            )
            .with_context(EventContext {
                workspace_id: self.workspace_id.clone(),
                session_id: Some(self.session_id.clone()),
                ..EventContext::default()
            }),
        );
    }
}

fn pack_compaction_sources(sources: Vec<String>, token_budget: usize) -> Vec<String> {
    let mut fragments = Vec::new();
    for source in sources {
        if estimate_text_tokens(&source) <= token_budget {
            fragments.push(source);
            continue;
        }
        let chars = source.chars().collect::<Vec<_>>();
        for (index, chunk) in chars.chunks(token_budget.max(1)).enumerate() {
            fragments.push(format!(
                "oversized_source_fragment={}\n{}",
                index,
                chunk.iter().collect::<String>()
            ));
        }
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_tokens = 0usize;
    for fragment in fragments {
        let fragment_tokens = estimate_text_tokens(&fragment);
        if !current.is_empty() && current_tokens.saturating_add(fragment_tokens) > token_budget {
            chunks.push(std::mem::take(&mut current));
            current_tokens = 0;
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(&fragment);
        current_tokens = current_tokens.saturating_add(fragment_tokens);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn estimate_thread_message_tokens(message: &ThreadChatMessage) -> usize {
    let mut total = estimate_text_tokens(&message.role) + 4;
    if let Some(content) = message.content.as_deref() {
        total += estimate_text_tokens(content);
    }
    if let Some(tool_call_id) = message.tool_call_id.as_deref() {
        total += estimate_text_tokens(tool_call_id);
    }
    for call in &message.tool_calls {
        total += estimate_text_tokens(&call.id);
        total += estimate_text_tokens(&call.kind);
        total += estimate_text_tokens(&call.function.name);
        total += estimate_text_tokens(&call.function.arguments);
    }
    for image in &message.images {
        total += estimate_text_tokens(&image.media_type) + 1_024;
    }
    total
}

pub(crate) fn estimate_thread_history_tokens(history: &[ThreadChatMessage]) -> usize {
    history.iter().map(estimate_thread_message_tokens).sum()
}

pub(crate) fn estimate_chat_messages_tokens(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|message| {
            let mut total = estimate_text_tokens(&message.role) + 4;
            if let Some(content) = message.content.as_deref() {
                total += estimate_text_tokens(content);
            }
            if let Some(tool_call_id) = message.tool_call_id.as_deref() {
                total += estimate_text_tokens(tool_call_id);
            }
            for call in &message.tool_calls {
                total += estimate_text_tokens(&call.id);
                total += estimate_text_tokens(&call.kind);
                total += estimate_text_tokens(&call.function.name);
                total += estimate_text_tokens(&call.function.arguments);
            }
            for image in &message.images {
                total += estimate_text_tokens(&image.media_type) + 1_024;
            }
            total
        })
        .sum()
}

pub(crate) fn estimate_tool_definition_tokens(tools: Option<&[ChatToolDefinition]>) -> usize {
    tools
        .and_then(|definitions| {
            let visible = definitions
                .iter()
                .map(|definition| {
                    serde_json::json!({
                        "type": definition.kind,
                        "function": definition.function,
                    })
                })
                .collect::<Vec<_>>();
            serde_json::to_string(&visible).ok()
        })
        .map(|serialized| estimate_text_tokens(&serialized))
        .unwrap_or_default()
}

fn validate_workspace_file_facts(history: &mut [ThreadChatMessage]) {
    for message in history.iter_mut().filter(|message| message.role == "tool") {
        let Some(content) = message.content.as_deref() else {
            continue;
        };
        let Ok(mut payload) = serde_json::from_str::<serde_json::Value>(content) else {
            continue;
        };
        if payload.get("tool").and_then(serde_json::Value::as_str) != Some("file_read") {
            continue;
        }
        let path = payload
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let expected_hash = payload
            .get("content_hash")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let is_current =
            path.as_deref()
                .zip(expected_hash.as_deref())
                .is_some_and(|(path, expected_hash)| {
                    magi_snapshot::path_content_hash(Path::new(path))
                        .is_ok_and(|actual_hash| actual_hash == expected_hash)
                });
        if is_current {
            payload["fact_state"] = serde_json::Value::String("current".to_string());
            message.content = Some(payload.to_string());
            continue;
        }
        message.content = Some(
            serde_json::json!({
                "tool": "file_read",
                "status": "stale",
                "fact_state": "stale",
                "path": path,
                "previous_content_hash": expected_hash,
                "reason": "workspace_content_changed",
                "message": "该文件读取事实已因工作区内容变化失效；如当前任务仍依赖它，必须重新读取。"
            })
            .to_string(),
        );
    }
}

pub(crate) fn current_session_file_facts(
    session_store: &SessionStore,
    session_id: &SessionId,
) -> Vec<CurrentFileFact> {
    let mut facts = BTreeMap::<String, (usize, CurrentFileFact)>::new();
    for thread in session_store.thread_registry_snapshot(session_id) {
        for message in thread
            .message_history
            .iter()
            .filter(|message| message.role == "tool")
        {
            let Some(result) = message.content.as_deref() else {
                continue;
            };
            let Ok(payload) = serde_json::from_str::<serde_json::Value>(result) else {
                continue;
            };
            if payload.get("tool").and_then(serde_json::Value::as_str) != Some("file_read")
                || infer_tool_call_status(result) != "success"
            {
                continue;
            }
            let (Some(path), Some(content_hash)) = (
                payload.get("path").and_then(serde_json::Value::as_str),
                payload
                    .get("content_hash")
                    .and_then(serde_json::Value::as_str),
            ) else {
                continue;
            };
            if !magi_snapshot::path_content_hash(Path::new(path))
                .is_ok_and(|actual_hash| actual_hash == content_hash)
            {
                continue;
            }
            let coverage = payload
                .get("bytes_read")
                .and_then(serde_json::Value::as_u64)
                .and_then(|bytes| usize::try_from(bytes).ok())
                .or_else(|| {
                    payload
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(str::len)
                })
                .unwrap_or(usize::MAX);
            let summary = payload
                .get("content")
                .and_then(serde_json::Value::as_str)
                .or_else(|| payload.get("summary").and_then(serde_json::Value::as_str))
                .unwrap_or("文件读取成功")
                .chars()
                .take(8_000)
                .collect::<String>();
            let fact = CurrentFileFact {
                absolute_path: path.to_string(),
                content_hash: content_hash.to_string(),
                result: result.to_string(),
                summary,
            };
            match facts.get(path) {
                Some((existing_coverage, _)) if *existing_coverage > coverage => {}
                _ => {
                    facts.insert(path.to_string(), (coverage, fact));
                }
            }
        }
    }
    facts.into_values().map(|(_, fact)| fact).collect()
}

fn checkpoint_file_facts_are_current(checkpoint: &ThreadContextCheckpoint) -> bool {
    checkpoint.file_fact_versions.iter().all(|fact| {
        magi_snapshot::path_content_hash(Path::new(&fact.path))
            .is_ok_and(|actual_hash| actual_hash == fact.content_hash)
    })
}

fn collect_file_fact_versions(
    history: &[ThreadChatMessage],
    previous: Option<&ThreadContextCheckpoint>,
) -> Vec<ThreadFileFactVersion> {
    let mut facts = previous
        .into_iter()
        .flat_map(|checkpoint| checkpoint.file_fact_versions.iter())
        .map(|fact| (fact.path.clone(), fact.content_hash.clone()))
        .collect::<BTreeMap<_, _>>();
    for message in history.iter().filter(|message| message.role == "tool") {
        let Some(content) = message.content.as_deref() else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(content) else {
            continue;
        };
        if payload.get("tool").and_then(serde_json::Value::as_str) != Some("file_read")
            || infer_tool_call_status(content) != "success"
        {
            continue;
        }
        if let (Some(path), Some(content_hash)) = (
            payload.get("path").and_then(serde_json::Value::as_str),
            payload
                .get("content_hash")
                .and_then(serde_json::Value::as_str),
        ) {
            facts.insert(path.to_string(), content_hash.to_string());
        }
    }
    facts
        .into_iter()
        .map(|(path, content_hash)| ThreadFileFactVersion { path, content_hash })
        .collect()
}

fn latest_session_usage_observation(
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
) -> Option<SessionRuntimeUsageObservation> {
    let snapshot = event_bus.audit_usage_ledger_snapshot();
    latest_usage_observations_from_ledger(&snapshot.usage_entries).remove(&session_id.to_string())
}

pub(crate) fn thread_history_compaction_decision(
    history: &[ThreadChatMessage],
    usage_observation: Option<&SessionRuntimeUsageObservation>,
    context_window_override: Option<u64>,
    additional_token_estimate: usize,
) -> Option<ThreadHistoryCompactionDecision> {
    let history_tokens = estimate_thread_history_tokens(history);
    let estimated_tokens = history_tokens.saturating_add(additional_token_estimate);
    if let Some(context_window) = context_window_override {
        let threshold_tokens = context_window.saturating_mul(AUTO_COMPACT_PERCENT as u64) / 100;
        let target_history_tokens =
            target_history_tokens_for_window(context_window, additional_token_estimate);
        let pressure_tokens = usage_observation
            .map(|observation| observation.context_window_tokens)
            .unwrap_or_default()
            .max(estimated_tokens as u64);
        if pressure_tokens >= threshold_tokens.max(1) && history_tokens > target_history_tokens {
            return Some(ThreadHistoryCompactionDecision::ContextWindowPressure {
                tokens_used: pressure_tokens,
                token_limit: context_window,
                threshold_tokens: threshold_tokens.max(1),
                target_history_tokens,
                resolved_model: usage_observation
                    .and_then(|observation| observation.resolved_model.clone()),
            });
        }
        return None;
    }
    if let Some(observation) = usage_observation {
        let context_window =
            resolve_context_window(observation.resolved_model.as_deref().unwrap_or(""));
        let threshold_tokens =
            (context_window.saturating_mul(AUTO_COMPACT_PERCENT) / 100).max(1) as u64;
        let target_history_tokens = target_history_tokens_for_window(
            context_window.max(0) as u64,
            additional_token_estimate,
        );
        if observation.context_window_tokens >= threshold_tokens
            && history_tokens > target_history_tokens
        {
            return Some(ThreadHistoryCompactionDecision::ContextWindowPressure {
                tokens_used: observation.context_window_tokens,
                token_limit: context_window.max(0) as u64,
                threshold_tokens,
                target_history_tokens,
                resolved_model: observation.resolved_model.clone(),
            });
        }
        if estimated_tokens >= threshold_tokens as usize && history_tokens > target_history_tokens {
            return Some(ThreadHistoryCompactionDecision::EstimatedPrefill {
                estimated_tokens,
                threshold_tokens: threshold_tokens as usize,
                target_history_tokens,
            });
        }
        return None;
    }
    let threshold_tokens = (DEFAULT_CONTEXT_WINDOW
        .saturating_mul(THREAD_HISTORY_ESTIMATED_PREFILL_COMPACT_PERCENT)
        / 100)
        .max(1) as usize;
    (estimated_tokens >= threshold_tokens && history_tokens > THREAD_HISTORY_COMPACT_TARGET_TOKENS)
        .then_some(ThreadHistoryCompactionDecision::EstimatedPrefill {
            estimated_tokens,
            threshold_tokens,
            target_history_tokens: THREAD_HISTORY_COMPACT_TARGET_TOKENS,
        })
}

fn target_history_tokens_for_window(
    context_window: u64,
    additional_token_estimate: usize,
) -> usize {
    let request_target =
        context_window.saturating_mul(THREAD_HISTORY_RECOVERY_TARGET_PERCENT) / 100;
    (request_target as usize)
        .saturating_sub(additional_token_estimate)
        .clamp(1, THREAD_HISTORY_COMPACT_TARGET_TOKENS)
}

fn thread_history_tail_is_tool_balanced(tail: &[ThreadChatMessage]) -> bool {
    let mut tool_call_ids = BTreeSet::new();
    let mut tool_result_ids = BTreeSet::new();
    for message in tail {
        for call in &message.tool_calls {
            tool_call_ids.insert(call.id.as_str());
        }
        if message.role == "tool" {
            let Some(tool_call_id) = message.tool_call_id.as_deref() else {
                return false;
            };
            if !tool_call_ids.contains(tool_call_id) {
                return false;
            }
            tool_result_ids.insert(tool_call_id);
        }
    }
    tool_call_ids
        .iter()
        .all(|tool_call_id| tool_result_ids.contains(tool_call_id))
}

fn choose_thread_history_compaction_split(
    history: &[ThreadChatMessage],
    target_history_tokens: usize,
) -> Option<usize> {
    if history.len() <= 1 {
        return None;
    }
    let target_tail = THREAD_HISTORY_RECENT_MESSAGE_TARGET.min(history.len().saturating_sub(1));
    let initial_split = history.len().saturating_sub(target_tail).max(1);
    let candidates = initial_split..history.len();
    candidates
        .clone()
        .find(|split| {
            thread_history_tail_is_tool_balanced(&history[*split..])
                && estimate_thread_history_tokens(&history[*split..]) <= target_history_tokens
        })
        .or_else(|| {
            candidates
                .rev()
                .find(|split| thread_history_tail_is_tool_balanced(&history[*split..]))
        })
}
