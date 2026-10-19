//! 模型调用的持久事实。请求正文仍由原有上下文/历史拥有，这里只保存不可变身份。
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
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelStepBinding {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub binding_revision: u32,
    pub settings_revision: u64,
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
});

impl ModelStep {
    pub fn validate_snapshot(&self) -> DomainResult<()> {
        if let Some(caps) = &self.executor_capabilities {
            caps.validate().map_err(|error| DomainError::Validation {
                message: error.to_string(),
            })?;
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
                    })))
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
        if self.attempts.len() != old.attempts.len() || self.status == ModelStepStatus::Started {
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
                            && previous.started_at == next.started_at)
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
}
