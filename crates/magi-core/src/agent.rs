//! Agent 身份附着于 Task；activation 的父子关系和资源仍由 TaskStore 拥有。
use crate::{DomainError, DomainResult, GoalId, SessionId, TaskId, UtcMillis};
use serde::{Deserialize, Serialize};

/// App Server agent control methods use JSON Schema `maxLength: 200`.
/// JSON Schema counts characters, so all Rust boundaries must use `chars()`
/// rather than the UTF-8 byte length.
pub const AGENT_OPERATION_ID_MAX_CHARS: usize = 200;

pub fn agent_operation_id_is_valid(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= AGENT_OPERATION_ID_MAX_CHARS
}

#[cfg(test)]
mod tests {
    use super::{AGENT_OPERATION_ID_MAX_CHARS, agent_operation_id_is_valid};

    #[test]
    fn operation_id_limit_counts_characters_across_protocol_boundaries() {
        assert!(agent_operation_id_is_valid(
            &"转".repeat(AGENT_OPERATION_ID_MAX_CHARS)
        ));
        assert!(!agent_operation_id_is_valid(
            &"转".repeat(AGENT_OPERATION_ID_MAX_CHARS + 1)
        ));
        assert!(!agent_operation_id_is_valid("   "));
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentLifetime {
    #[default]
    Turn,
    Session,
    Goal {
        goal_id: GoalId,
    },
}

/// 运行中子树的持久接管方，区别于 Agent 身份的保留期限。
/// Turn 不是转交目标；未转交的任务继续由创建它的 Turn 拥有。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentRunOwner {
    Session,
    Goal {
        goal_id: GoalId,
        control_revision: u64,
    },
}

/// 子树根上的唯一转交事实。父子链、Task/lease/Thread 和来源 Turn 均不改写；
/// 后代沿现有父链读取此记录，不复制另一棵资源树。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRunTransfer {
    pub operation_id: String,
    pub session_id: SessionId,
    pub source_turn_id: String,
    pub transferred_by: TaskId,
    pub activation_generation: u64,
    pub owner: AgentRunOwner,
    pub transferred_at: UtcMillis,
}

impl AgentRunTransfer {
    pub fn validate(&self, activation: &AgentActivation) -> DomainResult<()> {
        let lifetime_matches = match (&self.owner, &activation.lifetime) {
            (AgentRunOwner::Session, AgentLifetime::Session) => true,
            (
                AgentRunOwner::Goal {
                    goal_id,
                    control_revision,
                },
                AgentLifetime::Goal { goal_id: expected },
            ) => {
                goal_id == expected && *control_revision > 0 && !goal_id.as_str().trim().is_empty()
            }
            _ => false,
        };
        if !agent_operation_id_is_valid(&self.operation_id)
            || self.source_turn_id.trim().is_empty()
            || self.transferred_by.as_str().trim().is_empty()
            || self.session_id != activation.session_id
            || self.activation_generation != activation.generation
            || !lifetime_matches
        {
            return Err(DomainError::Validation {
                message: "Agent 子树转交身份、目标或来源无效".into(),
            });
        }
        Ok(())
    }
}

/// 每次激活的不可变来源。agent_id 是第一份 Task 的 ID，不创建第二棵代理树。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentActivation {
    pub agent_id: TaskId,
    pub session_id: SessionId,
    pub lifetime: AgentLifetime,
    pub generation: u64,
    pub previous_task_id: Option<TaskId>,
}

impl AgentActivation {
    pub fn validate(&self, task_id: &TaskId) -> DomainResult<()> {
        if self.agent_id.as_str().trim().is_empty()
            || self.session_id.as_str().trim().is_empty()
            || self.generation == 0
            || (self.generation == 1)
                != (self.agent_id == *task_id && self.previous_task_id.is_none())
            || (self.generation > 1
                && (self.previous_task_id.is_none()
                    || self.agent_id == *task_id
                    || self.previous_task_id.as_ref().is_some_and(|previous| {
                        previous == task_id || previous.as_str().trim().is_empty()
                    })))
            || matches!(&self.lifetime, AgentLifetime::Goal { goal_id } if goal_id.as_str().trim().is_empty())
        {
            return Err(DomainError::Validation {
                message: "Agent activation 身份或来源无效".into(),
            });
        }
        Ok(())
    }
}
