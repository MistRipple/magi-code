//! 模型调用的持久事实。请求正文仍由原有上下文/历史拥有，这里只保存不可变身份。
pub use magi_app_server_protocol::{
    ModelTransportOutcome, ModelTransportRecord, ModelTransportUsage,
};
use magi_core::{DomainError, DomainResult, GoalId, TaskId, ThreadId, UtcMillis};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelStepStatus {
    /// 已在调用 transport 前持久化；重启后须按结果未知处理，不能自动重放。
    Started,
    ResponseReceived,
    OutcomeUnknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelStepPurpose {
    Execution,
    ContextCompaction,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelAttempt {
    pub attempt_id: String,
    pub started_at: UtcMillis,
    pub settled_at: Option<UtcMillis>,
    pub status: ModelStepStatus,
    /// 老记录及非 HTTP 执行器缺失时保持未知，不补零值或伪造 wire 身份。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<ModelTransportRecord>,
}

fn valid_transport_update(
    old: Option<&ModelTransportRecord>,
    next: Option<&ModelTransportRecord>,
) -> bool {
    match (old, next) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(old), Some(next)) if old.outcome.is_some() => old == next,
        (Some(old), Some(next)) => {
            old.request_fingerprint == next.request_fingerprint
                && old.usage_observed.is_none_or(|observed| next.usage_observed.is_some_and(|next| !observed || next))
                && old.response_timing.as_ref().is_none_or(|timing| {
                    next.response_timing.as_ref() == Some(timing)
                        && next.usage == old.usage
                })
                && old
                    .observed_at
                    .is_none_or(|at| next.observed_at.is_some_and(|next| next >= at))
                && old
                    .http_status
                    .is_none_or(|status| next.http_status == Some(status))
                // Provider usage is a revisable snapshot while HTTP is in flight.
                // Identity / receipt order remain fixed; outcome freezes the entire receipt above.
                && old.usage.as_ref().is_none_or(|_| next.usage.is_some())
        }
    }
}

pub fn validate_model_transport_update(
    old: &ModelTransportRecord,
    next: &ModelTransportRecord,
) -> DomainResult<()> {
    if valid_transport_update(Some(old), Some(next)) {
        Ok(())
    } else {
        Err(DomainError::InvalidState {
            message: "模型 transport 证据已冻结或已观测用量丢失".into(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelStepBinding {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub binding_revision: u32,
    pub settings_revision: u64,
    /// 发送时冻结的脱敏统计绑定；恢复不得读取后来变更的连接或模型设置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_context: Option<magi_app_server_protocol::ModelUsageContext>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheInvalidationReason {
    #[default]
    Initial,
    ModelChanged,
    ProviderChanged,
    ReasoningChanged,
    PermissionChanged,
    SkillChanged,
    StablePrefixChanged,
    ToolSurfaceChanged,
    Compaction,
    HistoryRewritten,
    /// 工具结果总预算把更早的结果降级为摘要（降级边界按块前移）。
    ToolHistoryDemoted,
    /// 工作区文件已变化，历史中的文件读取事实由 current 变为 stale。
    FileFactsRefreshed,
    /// Provider 私有上下文（例如 reasoning signature）发生变化；它属于
    /// 已发送历史的 wire 事实，不能继续复用上一代前缀。
    ProviderContextChanged,
    /// 已授权 `contextRewrite` 的受信扩展改写了已装配历史（投影声明，见
    /// `HistoryProjection.extension_rewrites`）。改写生效、变化或被撤销都属于该原因。
    ExtensionRewrite,
    RendererChanged,
}

/// 受信扩展对模型可见历史的一次受控改写记录。canonical 日志与原文不变；
/// `original_digest` 是被替换消息序列化后的 SHA-256，用来回溯原文。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionRewriteRecord {
    pub plugin_id: String,
    pub contribution_id: String,
    /// 提出改写的钩子（`context.assemble` 或 `compaction.before`）。
    pub hook: String,
    /// 被替换的消息区间 [from, to)，下标相对改写前的装配历史。
    pub from: usize,
    pub to: usize,
    pub original_digest: String,
    pub replacement_digest: String,
}

/// 模型可见历史投影对已有结果的改写量。计数只增不减地描述“历史为何会被有意改写”，
/// 扩展改写记录则描述当前生效的改写集合；两者都用来把前缀变化归因到明确原因，
/// 没有任何声明变化的改写才是未声明的 `HistoryRewritten`。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryProjection {
    pub demoted_tool_results: usize,
    pub stale_file_facts: usize,
    /// 当前生效的扩展改写。读取缺失该字段的旧记录按“没有扩展改写”处理。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_rewrites: Vec<ExtensionRewriteRecord>,
}

/// 缓存身份只随原 ModelStep 事务提交；不建立第二份 lineage 状态或恢复日志。
/// 所有连接、权限与 Skill 字段均为摘要，不包含端点凭据及提示词正文。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelCacheBinding {
    pub lineage_id: String,
    pub cache_key: String,
    pub provider_hash: String,
    pub model_hash: String,
    pub reasoning_hash: String,
    pub execution_scope_hash: String,
    pub skill_scope_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_epoch_hash: Option<String>,
    /// None 表示同一代际继续追加；不是一次新的失效。
    pub invalidation_reason: Option<CacheInvalidationReason>,
}

impl ModelCacheBinding {
    fn validate(&self) -> bool {
        [
            &self.lineage_id,
            &self.cache_key,
            &self.provider_hash,
            &self.model_hash,
            &self.reasoning_hash,
            &self.execution_scope_hash,
            &self.skill_scope_hash,
        ]
        .into_iter()
        .all(|hash| valid_fingerprint(hash))
            && self
                .history_epoch_hash
                .as_deref()
                .is_none_or(valid_fingerprint)
    }
}

/// 发送前冻结的 Prompt IR 摘要。正文仍由上下文/历史事实拥有，Step 只保存
/// 能解释缓存断代和请求指纹的稳定摘要。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPromptFrame {
    pub ir_version: u32,
    pub stable_prefix_hash: String,
    #[serde(default)]
    pub append_only_history_hash: String,
    pub tool_surface_hash: String,
    pub volatile_tail_hash: String,
    /// irVersion >= 4：只摘要稳定前缀的私有上下文；历史归 historyChainHash，
    /// 易变尾部归 volatileTailHash。旧记录按其原 irVersion 保留，不重写。
    #[serde(default)]
    pub provider_context_hash: String,
    #[serde(default)]
    pub post_boundary_hash: String,
    /// 旧记录缺失时未知；有序哈希链用常量存储检测历史改写，避免逐 Step 复制历史。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_message_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_chain_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_projection: Option<HistoryProjection>,
    /// 序列化后易变尾部之前（工具表、系统规则、历史）的字节数；用于核算相邻请求的可复用前缀。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_bytes: Option<u64>,
    /// 序列化后易变尾部的字节数，每次请求都要重新发送的部分。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_binding: Option<ModelCacheBinding>,
}

impl ModelPromptFrame {
    fn validate(&self) -> bool {
        self.ir_version > 0
            && [
                &self.stable_prefix_hash,
                &self.append_only_history_hash,
                &self.tool_surface_hash,
                &self.volatile_tail_hash,
                &self.post_boundary_hash,
            ]
            .into_iter()
            .all(|hash| valid_fingerprint(hash))
            && (self.provider_context_hash.is_empty()
                || valid_fingerprint(&self.provider_context_hash))
            && self.history_message_count.is_some() == self.history_chain_hash.is_some()
            && (self.cache_binding.is_none() || self.history_message_count.is_some())
            && self
                .history_chain_hash
                .as_deref()
                .is_none_or(valid_fingerprint)
            && self
                .cache_binding
                .as_ref()
                .is_none_or(ModelCacheBinding::validate)
    }
}

persisted_without_task_budget!(ModelStep, "ModelStep", ["reservationTokens"], {
    pub step_id: String,
    pub attempts: Vec<ModelAttempt>,
    pub task_id: Option<TaskId>,
    /// 在发送前由 SessionStore 从 canonical Turn 归属解析，后续不依赖 Goal active 状态。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<GoalId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_tokens: Option<u64>,
    /// 同一 Goal 在本次结算后的累计用量；恢复取最大值，不重复增加 delta。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_tokens_after: Option<u64>,
    pub thread_id: ThreadId,
    pub checkpoint_generation: u64,
    pub request_fingerprint: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<ModelStepBinding>,
    /// 发送时冻结的可信宿主能力；旧历史缺失时保持未知，不能推断成新恢复授权。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor_capabilities: Option<magi_app_server_protocol::ExecutorCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<ModelStepPurpose>,
    pub message_count: usize,
    pub tool_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    pub input_request_ids: Vec<String>,
    pub started_at: UtcMillis,
    pub settled_at: Option<UtcMillis>,
    pub status: ModelStepStatus,
    pub response_fingerprint: Option<String>,
    /// Prompt IR 的稳定摘要；旧历史没有该字段时保持 None，恢复不能据此
    /// 猜测新的缓存身份。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_frame: Option<ModelPromptFrame>,
    /// 由 Prompt IR、模型绑定和执行权限共同计算的缓存断代。
    #[serde(default)]
    pub cache_epoch: u64,
});

pub fn validate_model_usage_context(
    context: &magi_app_server_protocol::ModelUsageContext,
) -> DomainResult<()> {
    let config: magi_usage_authority::LlmConfig = serde_json::from_value(
        serde_json::to_value(&context.model_config).expect("usage context serializes"),
    )
    .map_err(|error| DomainError::Validation {
        message: format!("模型统计绑定无效: {error}"),
    })?;
    let safe = magi_usage_authority::prepare_llm_config_for_persistence(config.clone());
    if context.template_id.trim().is_empty()
        || context.engine_id.trim().is_empty()
        || config.provider.trim().is_empty()
        || config.model.trim().is_empty()
        || safe.base_url != config.base_url
    {
        return Err(DomainError::Validation {
            message: "模型统计绑定包含无效身份或未脱敏连接".into(),
        });
    }

    Ok(())
}

impl ModelStep {
    /// 只累计提供方实际报告的用量；没有报告的 attempt 保持未知。
    pub fn observed_transport_tokens(&self) -> Option<u64> {
        self.attempts
            .iter()
            .filter_map(|attempt| attempt.transport.as_ref()?.usage.as_ref())
            .map(|usage| {
                magi_usage_authority::context_window_tokens_from_usage(
                    &magi_usage_authority::UsageTokenInput {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        total_tokens: None,
                        cache_read_tokens: usage.cache_read_tokens,
                        cache_write_tokens: usage.cache_write_tokens,
                        cache_read_included_in_input: usage.cache_read_included_in_input,
                    },
                )
            })
            .reduce(u64::saturating_add)
    }

    pub fn validate_snapshot(&self) -> DomainResult<()> {
        if let Some(context) = self
            .binding
            .as_ref()
            .and_then(|binding| binding.usage_context.as_ref())
        {
            validate_model_usage_context(context)?;
        }

        if let Some(caps) = &self.executor_capabilities {
            caps.validate().map_err(|error| DomainError::Validation {
                message: error.to_string(),
            })?;
        }
        if self
            .prompt_frame
            .as_ref()
            .is_some_and(|frame| !frame.validate())
        {
            return Err(DomainError::Validation {
                message: "模型 Step Prompt IR 摘要无效".into(),
            });
        }
        if self.attempts.is_empty() {
            return Err(DomainError::Validation {
                message: "模型 Step 缺少 transport attempt".into(),
            });
        }
        let mut last_at = self.started_at;
        for (index, attempt) in self.attempts.iter().enumerate() {
            if attempt.attempt_id != format!("{}:attempt:{}", self.step_id, index + 1)
                || (index == 0 && attempt.started_at != self.started_at)
                || attempt.started_at.0 < last_at.0
                || (attempt.status == ModelStepStatus::Started) != attempt.settled_at.is_none()
                || attempt
                    .settled_at
                    .is_some_and(|at| at.0 < attempt.started_at.0)
                || (index + 1 < self.attempts.len()
                    && attempt.status != ModelStepStatus::OutcomeUnknown)
                || attempt.transport.as_ref().is_some_and(|transport| {
                    !valid_fingerprint(&transport.request_fingerprint)
                        || transport
                            .http_status
                            .is_some_and(|status| !(100..600).contains(&status))
                })
            {
                return Err(DomainError::Validation {
                    message: "模型 attempt 顺序或状态无效".into(),
                });
            }
            last_at = attempt.settled_at.unwrap_or(attempt.started_at);
        }
        let last = self.attempts.last().expect("non-empty attempts");
        if last.status != self.status || last.settled_at != self.settled_at {
            return Err(DomainError::Validation {
                message: "Step 与当前 attempt 状态不一致".into(),
            });
        }
        if self.step_id.trim().is_empty()
            || self.thread_id.as_str().trim().is_empty()
            || self.provider.trim().is_empty()
            || self.max_output_tokens == Some(0)
            || (self.status != ModelStepStatus::ResponseReceived
                && (self.usage_tokens.is_some() || self.goal_tokens_after.is_some())
                && (self.status == ModelStepStatus::Started
                    || self.executor_capabilities.as_ref().is_none_or(|caps| {
                        caps.tools != magi_app_server_protocol::ExecutorToolMode::ExecutorOwned
                    }) && self.observed_transport_tokens() != self.usage_tokens))
            || self.goal_tokens_after.is_some_and(|after| {
                self.goal_id.is_none() || self.usage_tokens.is_none_or(|used| used > after)
            })
            || (self.goal_id.is_some()
                && self.usage_tokens.is_some()
                && self.goal_tokens_after.is_none())
            || !valid_fingerprint(&self.request_fingerprint)
            || self
                .response_fingerprint
                .as_ref()
                .is_some_and(|v| !valid_fingerprint(v))
            || (self.status == ModelStepStatus::Started) != self.settled_at.is_none()
            || (self.status == ModelStepStatus::ResponseReceived)
                != self.response_fingerprint.is_some()
            || self.settled_at.is_some_and(|at| at.0 < self.started_at.0)
            || self.input_request_ids.iter().any(|id| id.trim().is_empty())
            || self
                .input_request_ids
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.input_request_ids.len()
        {
            return Err(DomainError::Validation {
                message: "模型 Step 身份或状态无效".into(),
            });
        }
        Ok(())
    }

    /// Turn 已关闭时仅结算既有请求；不允许追加 attempt 或启动新调用。
    pub fn validate_terminal_settlement_from(&self, old: &Self) -> DomainResult<()> {
        self.validate_update_from(old)?;
        if self.attempts.len() != old.attempts.len()
            || (self.status == ModelStepStatus::Started
                && old
                    .attempts
                    .last()
                    .is_none_or(|attempt| attempt.transport.is_none()))
        {
            return Err(DomainError::InvalidState {
                message: "终态 Turn 只允许结算既有模型请求".into(),
            });
        }
        Ok(())
    }

    pub fn validate_update_from(&self, old: &Self) -> DomainResult<()> {
        self.validate_snapshot()?;
        let mut identity = self.clone();
        identity.attempts = old.attempts.clone();
        identity.status = old.status;
        identity.usage_tokens = old.usage_tokens;
        identity.goal_tokens_after = old.goal_tokens_after;
        identity.settled_at = old.settled_at;
        identity.response_fingerprint = old.response_fingerprint.clone();
        let attempts_valid = self.attempts.len() >= old.attempts.len()
            && old
                .attempts
                .iter()
                .zip(&self.attempts)
                .all(|(previous, next)| {
                    previous == next
                        || (previous.status == ModelStepStatus::Started
                            && previous.attempt_id == next.attempt_id
                            && previous.started_at == next.started_at
                            && valid_transport_update(
                                previous.transport.as_ref(),
                                next.transport.as_ref(),
                            ))
                });
        if !attempts_valid
            || &identity != old
            || (self != old && old.status != ModelStepStatus::Started)
        {
            return Err(DomainError::InvalidState {
                message: "模型 Step 请求已冻结或结果已结算".into(),
            });
        }
        Ok(())
    }
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn step() -> ModelStep {
        ModelStep {
            step_id: "step".into(),
            attempts: vec![ModelAttempt {
                transport: None,
                attempt_id: "step:attempt:1".into(),
                started_at: UtcMillis(10),
                settled_at: None,
                status: ModelStepStatus::Started,
            }],
            task_id: None,
            goal_id: None,
            usage_tokens: None,
            goal_tokens_after: None,
            thread_id: ThreadId::new("thread"),
            checkpoint_generation: 0,
            request_fingerprint: "a".repeat(64),
            provider: "test".into(),
            binding: None,
            executor_capabilities: None,
            purpose: None,
            message_count: 1,
            tool_count: 0,
            max_output_tokens: None,
            input_request_ids: vec![],
            started_at: UtcMillis(10),
            settled_at: None,
            status: ModelStepStatus::Started,
            response_fingerprint: None,
            prompt_frame: None,
            cache_epoch: 0,
        }
    }
    #[test]
    fn frozen_request_and_prior_attempts_cannot_be_rewritten() {
        let original = step();
        let mut next = original.clone();
        next.executor_capabilities = Some(serde_json::from_value(serde_json::json!({
            "inputModalities":["text"],"steer":"none","resume":"none","tools":"none","approvals":"unavailable","cancellation":"signal","context":"executor_owned","structuredOutput":false,"outputLimit":false,"background":false,"inspection":false
        })).unwrap());
        assert!(next.validate_update_from(&original).is_err());
        let mut malformed = next.clone();
        malformed
            .executor_capabilities
            .as_mut()
            .unwrap()
            .input_modalities
            .clear();
        assert!(malformed.validate_snapshot().is_err());
        let value = serde_json::to_value(&original).unwrap();
        assert!(value.get("executorCapabilities").is_none());
        assert_eq!(
            serde_json::from_value::<ModelStep>(value).unwrap(),
            original
        );
        next = original.clone();
        next.request_fingerprint = "b".repeat(64);
        assert!(next.validate_update_from(&original).is_err());
        next = original.clone();
        next.attempts[0].status = ModelStepStatus::OutcomeUnknown;
        next.attempts[0].settled_at = Some(UtcMillis(11));
        next.attempts.push(ModelAttempt {
            transport: None,
            attempt_id: "step:attempt:2".into(),
            started_at: UtcMillis(11),
            settled_at: None,
            status: ModelStepStatus::Started,
        });
        next.validate_update_from(&original).unwrap();
        let mut rewritten = next.clone();
        rewritten.attempts[0].settled_at = Some(UtcMillis(10));
        assert!(rewritten.validate_update_from(&next).is_err());
        rewritten = next.clone();
        rewritten.attempts.pop();
        assert!(rewritten.validate_update_from(&next).is_err());
    }

    #[test]
    fn transport_evidence_preserves_identity_and_freezes_at_settlement() {
        let original = step();
        let mut started = original.clone();
        started.attempts[0].transport = Some(ModelTransportRecord {
            usage_observed: None,
            response_timing: None,
            observed_at: None,
            request_fingerprint: "c".repeat(64),
            http_status: None,
            usage: None,
            outcome: None,
        });
        started.validate_update_from(&original).unwrap();
        let mut next = started.clone();
        let record = next.attempts[0].transport.as_mut().unwrap();
        record.http_status = Some(200);
        record.usage = Some(ModelTransportUsage {
            input_tokens: 20,
            output_tokens: 3,
            cache_read_tokens: Some(10),
            cache_write_tokens: None,
            cache_read_included_in_input: true,
        });
        next.validate_update_from(&started).unwrap();
        next.validate_terminal_settlement_from(&started).unwrap();
        let untimed = next.clone();
        next.attempts[0].transport.as_mut().unwrap().response_timing =
            Some(magi_app_server_protocol::ModelResponseTiming {
                response_duration_ms: Some(1500),
                first_content_ms: 300,
                content_duration_ms: 1000,
                text_output_tokens: Some(3),
            });
        next.validate_update_from(&untimed).unwrap();
        let mut lost_timing = next.clone();
        lost_timing.attempts[0]
            .transport
            .as_mut()
            .unwrap()
            .response_timing = None;
        assert!(lost_timing.validate_update_from(&next).is_err());
        let mut rewritten = next.clone();
        rewritten.attempts[0]
            .transport
            .as_mut()
            .unwrap()
            .request_fingerprint = "d".repeat(64);
        assert!(rewritten.validate_update_from(&next).is_err());
        rewritten = untimed.clone();
        rewritten.attempts[0]
            .transport
            .as_mut()
            .unwrap()
            .usage
            .as_mut()
            .unwrap()
            .output_tokens = 2;
        rewritten.validate_update_from(&untimed).unwrap();
        started = next.clone();
        next.attempts[0].transport.as_mut().unwrap().outcome = Some(ModelTransportOutcome::Failed);
        next.validate_update_from(&started).unwrap();
        rewritten = next.clone();
        rewritten.attempts[0].transport.as_mut().unwrap().outcome =
            Some(ModelTransportOutcome::Succeeded);
        assert!(rewritten.validate_update_from(&next).is_err());
        // 已关闭的 Turn 不能借元数据更新启动一个尚未冻结的 HTTP 请求。
        assert!(
            started
                .validate_terminal_settlement_from(&original)
                .is_err()
        );
    }
}
